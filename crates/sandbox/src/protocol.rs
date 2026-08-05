use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::limits::SandboxLimits;
use crate::request::{SandboxOutcome, SandboxRequest};

/// Largest frame either side will write or accept, in bytes.
///
/// This sits well above the largest legal payload - a request carries at most
/// `max_source_bytes` plus `max_stdin_bytes`, a response at most four capped
/// output sections - so hitting it means the peer is malfunctioning or hostile,
/// not that a run was unusually large.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Longest detail string carried on an error frame.
const MAX_DETAIL_BYTES: usize = 256;

/// One request from the API to the playground daemon.
///
/// Serde cannot apply `deny_unknown_fields` to an internally tagged enum, so a
/// stray key alongside `operation` is ignored rather than refused. The payload
/// inside is a plain struct and stays strict, which is where a skewed peer
/// would actually do damage.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum Request {
    /// Compile and optionally run a submission.
    Run {
        /// The submission itself.
        request: Box<SandboxRequest>,
    },
    /// Report the limits every run is subject to.
    Limits,
}

/// One response from the playground daemon to the API.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Response {
    /// A run completed, whether or not the build succeeded.
    Completed {
        /// What the run produced.
        outcome: Box<SandboxOutcome>,
    },
    /// The limits every run is subject to.
    Limits {
        /// The resource envelope.
        limits: SandboxLimits,
    },
    /// The request could not be carried out.
    Failed {
        /// What kind of failure this was.
        kind: FailureKind,
        /// A content-free explanation, when one is safe to give.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl Response {
    /// Builds a failure frame, bounding the detail string.
    ///
    /// The detail must never carry submitted source or standard input. The only
    /// values passed here in practice come from [`crate::SourceError`], whose
    /// variants carry offsets and sizes but never the offending text.
    #[must_use]
    pub fn failed(kind: FailureKind, detail: Option<&str>) -> Self {
        Self::Failed {
            kind,
            detail: detail.map(|detail| truncate_detail(detail).to_owned()),
        }
    }
}

/// Why a request could not be carried out.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The submission was rejected before any container was started.
    InvalidRequest,
    /// The daemon is saturated or the container runtime is not answering.
    Unavailable,
    /// The run overran its deadline.
    Timeout,
    /// Something inside the daemon went wrong.
    Internal,
}

impl FailureKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
        }
    }
}

/// Writes one newline-terminated JSON frame.
///
/// # Errors
///
/// Returns [`ProtocolError::TooLarge`] when the encoded value would exceed
/// [`MAX_FRAME_BYTES`], and [`ProtocolError::Io`] when the write fails.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), ProtocolError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut encoded = serde_json::to_vec(value).map_err(|_| ProtocolError::Malformed)?;
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge);
    }

    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;

    Ok(())
}

/// Reads one newline-terminated JSON frame, refusing anything oversized.
///
/// The cap is applied while reading rather than after, so a peer cannot make
/// the reader allocate without bound by never sending a newline.
///
/// # Errors
///
/// Returns [`ProtocolError::Closed`] at a clean end of stream,
/// [`ProtocolError::TooLarge`] when the frame exceeds [`MAX_FRAME_BYTES`],
/// [`ProtocolError::Truncated`] when the stream ends mid-frame, and
/// [`ProtocolError::Malformed`] when the frame is not the expected JSON.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, ProtocolError>
where
    R: AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    let mut buffer = Vec::new();
    let ceiling = MAX_FRAME_BYTES + 1;
    let mut limited = reader.take(ceiling as u64);
    limited.read_until(b'\n', &mut buffer).await?;

    if buffer.is_empty() {
        return Err(ProtocolError::Closed);
    }

    if buffer.last() != Some(&b'\n') {
        // A full read with no terminator is an oversized frame; a short one is
        // a peer that went away mid-write.
        return Err(if buffer.len() >= ceiling {
            ProtocolError::TooLarge
        } else {
            ProtocolError::Truncated
        });
    }

    buffer.pop();
    serde_json::from_slice(&buffer).map_err(|_| ProtocolError::Malformed)
}

/// Cuts a detail string to its cap on a character boundary.
fn truncate_detail(detail: &str) -> &str {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail;
    }

    let mut end = MAX_DETAIL_BYTES;
    while end > 0 && !detail.is_char_boundary(end) {
        end -= 1;
    }

    &detail[..end]
}

/// A frame that could not be exchanged.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// The peer closed the connection before sending anything.
    #[error("the peer closed the connection")]
    Closed,
    /// The peer stopped partway through a frame.
    #[error("the frame ended early")]
    Truncated,
    /// The frame exceeded the protocol's ceiling.
    #[error("the frame exceeds the protocol limit")]
    TooLarge,
    /// The frame was not the expected JSON document.
    #[error("the frame is not a valid protocol document")]
    Malformed,
    /// The underlying stream failed.
    #[error("the connection failed")]
    Io(#[from] std::io::Error),
}

/// Reads the whole of `reader` as UTF-8, used only by tests.
#[cfg(test)]
async fn drain<R: tokio::io::AsyncRead + Unpin>(mut reader: R) -> String {
    let mut text = String::new();
    reader
        .read_to_string(&mut text)
        .await
        .expect("draining a duplex should succeed");
    text
}

#[cfg(test)]
mod tests {
    use tokio::io::BufReader;

    use super::{
        FailureKind, MAX_FRAME_BYTES, ProtocolError, Request, Response, drain, read_frame,
        write_frame,
    };
    use crate::limits::SandboxLimits;
    use crate::request::{SandboxMode, SandboxOutcome, SandboxProfile, SandboxRequest};

    fn run_request(source: &str) -> Request {
        Request::Run {
            request: Box::new(SandboxRequest {
                mode: SandboxMode::Run,
                profile: SandboxProfile::Debug,
                source: source.to_owned(),
                stdin: String::new(),
            }),
        }
    }

    #[tokio::test]
    async fn a_request_survives_a_round_trip_over_a_duplex_pair() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        let sent = run_request("Fn Main() {}\n");

        write_frame(&mut client, &sent).await.expect("frame writes");
        drop(client);

        let mut reader = BufReader::new(server);
        let received: Request = read_frame(&mut reader).await.expect("frame reads");

        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn every_response_shape_survives_a_round_trip() {
        let responses = [
            Response::Completed {
                outcome: Box::new(SandboxOutcome::default()),
            },
            Response::Limits {
                limits: SandboxLimits::default(),
            },
            Response::failed(FailureKind::Unavailable, None),
            Response::failed(FailureKind::InvalidRequest, Some("source cannot be empty")),
        ];

        for sent in responses {
            let (mut client, server) = tokio::io::duplex(64 * 1024);
            write_frame(&mut client, &sent).await.expect("frame writes");
            drop(client);

            let mut reader = BufReader::new(server);
            let received: Response = read_frame(&mut reader).await.expect("frame reads");

            assert_eq!(received, sent);
        }
    }

    #[tokio::test]
    async fn a_frame_is_exactly_one_line() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);

        write_frame(&mut client, &Request::Limits)
            .await
            .expect("frame writes");
        drop(client);

        let text = drain(server).await;
        assert_eq!(text, "{\"operation\":\"limits\"}\n");
        assert_eq!(text.matches('\n').count(), 1);
    }

    #[tokio::test]
    async fn two_frames_can_be_read_back_to_back_from_one_stream() {
        let (mut client, server) = tokio::io::duplex(64 * 1024);
        write_frame(&mut client, &Request::Limits).await.unwrap();
        write_frame(&mut client, &run_request("x")).await.unwrap();
        drop(client);

        let mut reader = BufReader::new(server);
        let first: Request = read_frame(&mut reader).await.expect("first frame reads");
        let second: Request = read_frame(&mut reader).await.expect("second frame reads");

        assert_eq!(first, Request::Limits);
        assert_eq!(second, run_request("x"));
    }

    #[tokio::test]
    async fn a_clean_end_of_stream_is_reported_as_closed() {
        let (client, server) = tokio::io::duplex(1024);
        drop(client);

        let mut reader = BufReader::new(server);
        let outcome: Result<Request, _> = read_frame(&mut reader).await;

        assert!(matches!(outcome, Err(ProtocolError::Closed)));
    }

    #[tokio::test]
    async fn a_frame_that_ends_mid_write_is_reported_as_truncated() {
        let (mut client, server) = tokio::io::duplex(1024);
        tokio::io::AsyncWriteExt::write_all(&mut client, b"{\"operation\":\"lim")
            .await
            .expect("partial write should succeed");
        drop(client);

        let mut reader = BufReader::new(server);
        let outcome: Result<Request, _> = read_frame(&mut reader).await;

        assert!(matches!(outcome, Err(ProtocolError::Truncated)));
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused_while_reading_rather_than_after() {
        let (client, server) = tokio::io::duplex(64 * 1024);

        // A peer that never sends a newline must not make the reader grow
        // without bound, so this writer is deliberately endless.
        let writer = tokio::spawn(async move {
            let mut client = client;
            let chunk = vec![b'x'; 16 * 1024];
            loop {
                if tokio::io::AsyncWriteExt::write_all(&mut client, &chunk)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut reader = BufReader::new(server);
        let outcome: Result<Request, _> = read_frame(&mut reader).await;

        assert!(matches!(outcome, Err(ProtocolError::TooLarge)));
        writer.abort();
    }

    #[tokio::test]
    async fn a_frame_at_exactly_the_ceiling_is_still_accepted() {
        let (mut client, server) = tokio::io::duplex(2 * MAX_FRAME_BYTES);
        // Sized so the encoded document lands just inside the ceiling.
        let padding = MAX_FRAME_BYTES - 128;
        let sent = run_request(&"x".repeat(padding));

        write_frame(&mut client, &sent)
            .await
            .expect("a frame inside the ceiling should write");
        drop(client);

        let mut reader = BufReader::new(server);
        let received: Request = read_frame(&mut reader).await.expect("frame reads");

        assert_eq!(received, sent);
    }

    #[tokio::test]
    async fn writing_an_oversized_frame_fails_instead_of_emitting_it() {
        let (mut client, server) = tokio::io::duplex(1024);
        let sent = run_request(&"x".repeat(2 * MAX_FRAME_BYTES));

        let outcome = write_frame(&mut client, &sent).await;

        assert!(matches!(outcome, Err(ProtocolError::TooLarge)));
        drop(client);
        assert!(
            drain(server).await.is_empty(),
            "nothing may reach the peer when the frame is refused"
        );
    }

    #[tokio::test]
    async fn malformed_and_unexpected_documents_are_refused() {
        for line in [
            "not json at all\n",
            "{}\n",
            "{\"operation\":\"nonsense\"}\n",
            "[]\n",
            // The submission inside the envelope stays strict.
            "{\"operation\":\"run\",\"request\":{\"mode\":\"run\",\"profile\":\"debug\",\"source\":\"x\",\"extra\":1}}\n",
        ] {
            let (mut client, server) = tokio::io::duplex(1024);
            tokio::io::AsyncWriteExt::write_all(&mut client, line.as_bytes())
                .await
                .expect("write should succeed");
            drop(client);

            let mut reader = BufReader::new(server);
            let outcome: Result<Request, _> = read_frame(&mut reader).await;

            assert!(
                matches!(outcome, Err(ProtocolError::Malformed)),
                "expected {line:?} to be refused"
            );
        }
    }

    #[test]
    fn a_failure_detail_is_bounded() {
        let Response::Failed { detail, .. } =
            Response::failed(FailureKind::Internal, Some(&"e".repeat(4096)))
        else {
            panic!("expected a failure response");
        };

        assert_eq!(detail.expect("detail should be present").len(), 256);
    }

    #[test]
    fn failure_kinds_use_stable_snake_case_names() {
        assert_eq!(FailureKind::InvalidRequest.as_str(), "invalid_request");
        assert_eq!(
            serde_json::to_string(&FailureKind::Timeout).unwrap(),
            "\"timeout\""
        );
    }
}
