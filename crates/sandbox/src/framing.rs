use thiserror::Error;

use crate::job::Nonce;

/// Byte that introduces a section sentinel line: ASCII record separator.
pub const SECTION_SENTINEL: u8 = 0x1e;

/// The sections the container entry point may emit, in the order it emits them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    /// Compiler diagnostics.
    Build,
    /// Program standard output.
    Stdout,
    /// Program standard error.
    Stderr,
    /// Reformatted source.
    Formatted,
    /// Trailing machine-readable run status.
    Status,
}

impl Section {
    const ALL: [Self; 5] = [
        Self::Build,
        Self::Stdout,
        Self::Stderr,
        Self::Formatted,
        Self::Status,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Formatted => "formatted",
            Self::Status => "status",
        }
    }

    fn from_bytes(value: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|section| section.as_str().as_bytes() == value)
    }
}

/// The raw section bodies recovered from one run's standard output.
///
/// Bodies are byte slices because a program may print anything at all; they
/// become strings only through [`truncate`], which bounds them first.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FramedOutput<'a> {
    /// Compiler diagnostics, empty when the container emitted none.
    pub build: &'a [u8],
    /// Program standard output.
    pub stdout: &'a [u8],
    /// Program standard error.
    pub stderr: &'a [u8],
    /// Reformatted source.
    pub formatted: &'a [u8],
    /// Parsed trailing status.
    pub status: RunStatus,
}

/// The machine-readable trailer the entry point writes last.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RunStatus {
    /// Exit code of the compile step.
    pub build_exit_code: i32,
    /// Wall-clock duration of the compile step.
    pub build_duration_ms: u64,
    /// Exit code of the program step, when one ran and was not signalled.
    pub run_exit_code: Option<i32>,
    /// Signal that killed the program step, when one did.
    pub run_signal: Option<i32>,
    /// Wall-clock duration of the program step, when one ran.
    pub run_duration_ms: Option<u64>,
    /// Whether a step hit its in-container timeout.
    pub timed_out: bool,
}

/// Splits a run's standard output into its sections.
///
/// A line is a sentinel only when it is exactly `\x1e<nonce>:<section>`. The
/// nonce is generated per run and never given to the program, so output that
/// merely looks like a sentinel — including a correct-looking one carrying a
/// guessed nonce — stays part of the section being read. Sections named with a
/// valid nonce but an unrecognized name are discarded, which lets the image add
/// a section before the server learns to read it.
///
/// # Errors
///
/// Returns a [`FramingError`] when the required `status` section is missing,
/// when a section is emitted twice, or when the status body is malformed.
pub fn parse_framed_output<'a>(
    stdout: &'a [u8],
    nonce: &Nonce,
) -> Result<FramedOutput<'a>, FramingError> {
    let mut bodies: [Option<&'a [u8]>; 5] = [None; 5];
    let mut current: Option<Section> = None;
    let mut body_start = 0usize;
    let mut cursor = 0usize;

    while cursor < stdout.len() {
        let line_end = memchr(stdout, cursor, b'\n');
        let line = &stdout[cursor..line_end];
        let next = if line_end < stdout.len() {
            line_end + 1
        } else {
            line_end
        };

        match classify_line(line, nonce) {
            Line::Body => {}
            Line::Sentinel(section) => {
                if let Some(previous) = current {
                    store(&mut bodies, previous, &stdout[body_start..cursor])?;
                }
                current = section;
                body_start = next;
            }
        }

        cursor = next;
    }

    if let Some(previous) = current {
        store(&mut bodies, previous, &stdout[body_start..])?;
    }

    let status_body = bodies[index(Section::Status)].ok_or(FramingError::MissingStatus)?;

    Ok(FramedOutput {
        build: bodies[index(Section::Build)].unwrap_or_default(),
        stdout: bodies[index(Section::Stdout)].unwrap_or_default(),
        stderr: bodies[index(Section::Stderr)].unwrap_or_default(),
        formatted: bodies[index(Section::Formatted)].unwrap_or_default(),
        status: parse_status(status_body)?,
    })
}

/// What one line of container output turned out to be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Line {
    /// Ordinary content, belonging to whichever section is open.
    Body,
    /// A sentinel bearing this run's nonce. The payload is `None` when it names
    /// a section this build does not know, whose body is then discarded.
    Sentinel(Option<Section>),
}

/// Decides whether `line` opens a new section.
///
/// Anything that is not exactly `\x1e<nonce>:<name>` is body content, including
/// a sentinel carrying a nonce the program guessed.
fn classify_line(line: &[u8], nonce: &Nonce) -> Line {
    let Some(rest) = line.strip_prefix(&[SECTION_SENTINEL]) else {
        return Line::Body;
    };
    // Tolerate a trailing carriage return so the framing survives a container
    // whose shell writes CRLF.
    let rest = rest.strip_suffix(b"\r").unwrap_or(rest);
    let Some(rest) = rest.strip_prefix(nonce.as_str().as_bytes()) else {
        return Line::Body;
    };
    let Some(name) = rest.strip_prefix(b":") else {
        return Line::Body;
    };

    Line::Sentinel(Section::from_bytes(name))
}

fn store<'a>(
    bodies: &mut [Option<&'a [u8]>; 5],
    section: Section,
    body: &'a [u8],
) -> Result<(), FramingError> {
    let slot = &mut bodies[index(section)];
    if slot.is_some() {
        return Err(FramingError::DuplicateSection { section });
    }
    *slot = Some(body);
    Ok(())
}

const fn index(section: Section) -> usize {
    match section {
        Section::Build => 0,
        Section::Stdout => 1,
        Section::Stderr => 2,
        Section::Formatted => 3,
        Section::Status => 4,
    }
}

fn memchr(haystack: &[u8], from: usize, needle: u8) -> usize {
    haystack[from..]
        .iter()
        .position(|byte| *byte == needle)
        .map_or(haystack.len(), |offset| from + offset)
}

/// Parses the `key=value` lines of the status section.
fn parse_status(body: &[u8]) -> Result<RunStatus, FramingError> {
    let body = std::str::from_utf8(body).map_err(|_| FramingError::MalformedStatus)?;
    let mut status = RunStatus::default();
    let mut build_exit_seen = false;
    let mut build_duration_seen = false;

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let (key, value) = line.split_once('=').ok_or(FramingError::MalformedStatus)?;
        match key {
            "build_exit" => {
                status.build_exit_code = exit_code(value)?;
                build_exit_seen = true;
            }
            "build_ms" => {
                status.build_duration_ms = duration(value)?;
                build_duration_seen = true;
            }
            "run_exit" => status.run_exit_code = Some(exit_code(value)?),
            "run_signal" => status.run_signal = nonzero_signal(value)?,
            "run_ms" => status.run_duration_ms = Some(duration(value)?),
            "timed_out" => status.timed_out = flag(value)?,
            // Unknown keys are ignored so the image can report more than the
            // server currently reads.
            _ => {}
        }
    }

    if !build_exit_seen || !build_duration_seen {
        return Err(FramingError::MalformedStatus);
    }

    // A signalled program has no meaningful exit code.
    if status.run_signal.is_some() {
        status.run_exit_code = None;
    }

    Ok(status)
}

fn exit_code(value: &str) -> Result<i32, FramingError> {
    match value.parse::<i32>() {
        Ok(code) if (0..=255).contains(&code) => Ok(code),
        _ => Err(FramingError::MalformedStatus),
    }
}

fn nonzero_signal(value: &str) -> Result<Option<i32>, FramingError> {
    match value.parse::<i32>() {
        Ok(0) => Ok(None),
        Ok(signal) if (1..=64).contains(&signal) => Ok(Some(signal)),
        _ => Err(FramingError::MalformedStatus),
    }
}

fn duration(value: &str) -> Result<u64, FramingError> {
    value
        .parse::<u64>()
        .map_err(|_| FramingError::MalformedStatus)
}

fn flag(value: &str) -> Result<bool, FramingError> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(FramingError::MalformedStatus),
    }
}

/// Bounds `bytes` to `cap` and decodes it as UTF-8.
///
/// The cut lands on a character boundary, so a multi-byte sequence is dropped
/// whole rather than left half-written. Bytes that were not valid UTF-8 to
/// begin with — a program is free to print anything — are replaced rather than
/// rejected.
///
/// Returns the text and whether anything was dropped.
#[must_use]
pub fn truncate(bytes: &[u8], cap: usize) -> (String, bool) {
    if bytes.len() <= cap {
        return (String::from_utf8_lossy(bytes).into_owned(), false);
    }

    let mut end = cap;
    // A UTF-8 continuation byte is `10xxxxxx`; step back off a partial sequence.
    while end > 0 && bytes[end] & 0b1100_0000 == 0b1000_0000 {
        end -= 1;
    }

    (String::from_utf8_lossy(&bytes[..end]).into_owned(), true)
}

/// A run whose framed output could not be interpreted.
///
/// Variants name structure only, never the surrounding output.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FramingError {
    /// The container did not emit the trailing status section.
    #[error("run output is missing its status section")]
    MissingStatus,
    /// The status section could not be parsed.
    #[error("run output has a malformed status section")]
    MalformedStatus,
    /// A section was emitted more than once.
    #[error("run output repeats the {} section", section.as_str())]
    DuplicateSection { section: Section },
}

#[cfg(test)]
mod tests {
    use super::{FramingError, Section, parse_framed_output, truncate};
    use crate::job::Nonce;

    const NONCE: &str = "fedcba9876543210fedcba9876543210";
    const OTHER: &str = "00000000000000000000000000000000";

    fn nonce() -> Nonce {
        Nonce::new(NONCE).unwrap()
    }

    fn sentinel(section: &str) -> String {
        format!("\u{1e}{NONCE}:{section}\n")
    }

    fn status_section() -> String {
        format!("{}build_exit=0\nbuild_ms=42\n", sentinel("status"))
    }

    #[test]
    fn sections_are_split_at_their_sentinels() {
        let output = format!(
            "{}warning: unused\n{}hello\n{}oops\n{}",
            sentinel("build"),
            sentinel("stdout"),
            sentinel("stderr"),
            status_section()
        );

        let framed = parse_framed_output(output.as_bytes(), &nonce()).unwrap();

        assert_eq!(framed.build, b"warning: unused\n");
        assert_eq!(framed.stdout, b"hello\n");
        assert_eq!(framed.stderr, b"oops\n");
        assert_eq!(framed.formatted, b"");
    }

    #[test]
    fn output_before_the_first_sentinel_is_discarded() {
        let output = format!("noise from the runtime\n{}", status_section());

        let framed = parse_framed_output(output.as_bytes(), &nonce()).unwrap();

        assert_eq!(framed.build, b"");
        assert_eq!(framed.status.build_duration_ms, 42);
    }

    #[test]
    fn a_program_cannot_forge_a_section_with_a_guessed_nonce() {
        let forged = format!(
            "{}real output\n\u{1e}{OTHER}:stderr\nforged as stderr\n\u{1e}:stderr\n\u{1e}{NONCE}stderr\n{}",
            sentinel("stdout"),
            status_section()
        );

        let framed = parse_framed_output(forged.as_bytes(), &nonce()).unwrap();

        assert_eq!(framed.stderr, b"", "no forged section may be accepted");
        assert!(
            std::str::from_utf8(framed.stdout)
                .unwrap()
                .contains("forged as stderr"),
            "forged sentinels stay inside the section being read"
        );
    }

    #[test]
    fn a_sentinel_must_occupy_a_whole_line() {
        let output = format!(
            "{}prefix \u{1e}{NONCE}:stderr trailing\n{}",
            sentinel("stdout"),
            status_section()
        );

        let framed = parse_framed_output(output.as_bytes(), &nonce()).unwrap();

        assert_eq!(framed.stderr, b"");
        assert!(framed.stdout.starts_with(b"prefix "));
    }

    #[test]
    fn unknown_section_names_with_a_valid_nonce_are_discarded() {
        let output = format!(
            "{}kept\n{}dropped\n{}",
            sentinel("stdout"),
            sentinel("coverage"),
            status_section()
        );

        let framed = parse_framed_output(output.as_bytes(), &nonce()).unwrap();

        assert_eq!(framed.stdout, b"kept\n");
        assert_eq!(framed.status.build_exit_code, 0);
    }

    #[test]
    fn a_missing_status_section_is_an_error() {
        let output = format!("{}hello\n", sentinel("stdout"));

        assert_eq!(
            parse_framed_output(output.as_bytes(), &nonce()).unwrap_err(),
            FramingError::MissingStatus
        );
        assert_eq!(
            parse_framed_output(b"", &nonce()).unwrap_err(),
            FramingError::MissingStatus
        );
    }

    #[test]
    fn a_repeated_section_is_an_error() {
        let output = format!(
            "{}one\n{}two\n{}",
            sentinel("stdout"),
            sentinel("stdout"),
            status_section()
        );

        assert_eq!(
            parse_framed_output(output.as_bytes(), &nonce()).unwrap_err(),
            FramingError::DuplicateSection {
                section: Section::Stdout,
            }
        );
    }

    #[test]
    fn a_full_status_section_is_parsed() {
        let output = format!(
            "{}build_exit=0\nbuild_ms=120\nrun_exit=3\nrun_signal=0\nrun_ms=8\ntimed_out=0\n",
            sentinel("status")
        );

        let status = parse_framed_output(output.as_bytes(), &nonce())
            .unwrap()
            .status;

        assert_eq!(status.build_exit_code, 0);
        assert_eq!(status.build_duration_ms, 120);
        assert_eq!(status.run_exit_code, Some(3));
        assert_eq!(status.run_signal, None);
        assert_eq!(status.run_duration_ms, Some(8));
        assert!(!status.timed_out);
    }

    #[test]
    fn a_signalled_program_reports_the_signal_and_no_exit_code() {
        let output = format!(
            "{}build_exit=0\nbuild_ms=1\nrun_exit=137\nrun_signal=9\ntimed_out=1\n",
            sentinel("status")
        );

        let status = parse_framed_output(output.as_bytes(), &nonce())
            .unwrap()
            .status;

        assert_eq!(status.run_signal, Some(9));
        assert_eq!(status.run_exit_code, None);
        assert!(status.timed_out);
    }

    #[test]
    fn malformed_or_incomplete_status_bodies_are_rejected() {
        for body in [
            "build_ms=1\n",                     // no build_exit
            "build_exit=0\n",                   // no build_ms
            "build_exit=0\nbuild_ms=abc\n",     // non-numeric duration
            "build_exit=999\nbuild_ms=1\n",     // exit code out of range
            "build_exit=0\nbuild_ms=1\nnope\n", // not a key=value pair
            "build_exit=0\nbuild_ms=1\ntimed_out=maybe\n",
        ] {
            let output = format!("{}{body}", sentinel("status"));

            assert_eq!(
                parse_framed_output(output.as_bytes(), &nonce()).unwrap_err(),
                FramingError::MalformedStatus,
                "expected {body:?} to be rejected"
            );
        }
    }

    #[test]
    fn framing_survives_carriage_returns_on_sentinel_lines() {
        let output = format!(
            "\u{1e}{NONCE}:stdout\r\nhello\n\u{1e}{NONCE}:status\r\nbuild_exit=0\nbuild_ms=1\n"
        );

        let framed = parse_framed_output(output.as_bytes(), &nonce()).unwrap();

        assert_eq!(framed.stdout, b"hello\n");
    }

    #[test]
    fn truncate_leaves_short_output_untouched() {
        assert_eq!(truncate(b"hello", 16), ("hello".to_owned(), false));
        assert_eq!(truncate(b"hello", 5), ("hello".to_owned(), false));
        assert_eq!(truncate(b"", 0), (String::new(), false));
    }

    #[test]
    fn truncate_cuts_on_a_character_boundary_rather_than_mid_sequence() {
        // Three-byte characters, so caps of 4, 5, and 6 all land inside one.
        let bytes = "日本語".as_bytes();

        assert_eq!(truncate(bytes, 4), ("日".to_owned(), true));
        assert_eq!(truncate(bytes, 5), ("日".to_owned(), true));
        assert_eq!(truncate(bytes, 6), ("日本".to_owned(), true));
        assert_eq!(truncate(bytes, 3), ("日".to_owned(), true));
    }

    #[test]
    fn truncate_reports_the_cut_and_never_yields_a_replacement_from_its_own_cut() {
        let bytes = "aé".as_bytes(); // 1 + 2 bytes
        let (text, truncated) = truncate(bytes, 2);

        assert_eq!(text, "a");
        assert!(truncated);
        assert!(!text.contains('\u{fffd}'));
    }

    #[test]
    fn truncate_replaces_bytes_that_were_never_valid_utf8() {
        let (text, truncated) = truncate(&[b'a', 0xff, b'b'], 16);

        assert_eq!(text, "a\u{fffd}b");
        assert!(!truncated);
    }
}
