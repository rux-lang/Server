use std::fmt::Write;

use async_trait::async_trait;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{ChecksumAlgorithm, ObjectCannedAcl};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rux_application::{
    ArtifactStorage, ArtifactStorageError, ArtifactStorageErrorKind, ArtifactUpload,
    ObjectVersionCursor, ObjectVersionPage, ObjectVersionStorageError,
    ObjectVersionStorageErrorKind, PACKAGE_OBJECT_PREFIX, StoredArtifact, StoredObjectVersion,
    VersionedArtifactStorage,
};
use time::OffsetDateTime;

const ARTIFACT_CONTENT_TYPE: &str = "application/zip";
const ARTIFACT_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";

/// Stores public immutable package artifacts through an S3-compatible API.
#[derive(Clone)]
pub struct SpacesArtifactStorage {
    client: S3Client,
    bucket: String,
}

impl SpacesArtifactStorage {
    #[must_use]
    pub const fn new(client: S3Client, bucket: String) -> Self {
        Self { client, bucket }
    }
}

#[async_trait]
impl ArtifactStorage for SpacesArtifactStorage {
    async fn store(
        &self,
        artifact: ArtifactUpload,
    ) -> Result<StoredArtifact, ArtifactStorageError> {
        let content_length = i64::try_from(artifact.byte_size).map_err(|error| {
            ArtifactStorageError::with_source(ArtifactStorageErrorKind::SourceUnavailable, error)
        })?;
        let body = ByteStream::from_path(&artifact.path)
            .await
            .map_err(|error| {
                ArtifactStorageError::with_source(
                    ArtifactStorageErrorKind::SourceUnavailable,
                    error,
                )
            })?;
        let checksum = STANDARD.encode(artifact.sha256.as_bytes());
        let storage_key = storage_key(&artifact);

        let output = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&storage_key)
            .body(body)
            .content_length(content_length)
            .content_type(ARTIFACT_CONTENT_TYPE)
            .cache_control(ARTIFACT_CACHE_CONTROL)
            .acl(ObjectCannedAcl::PublicRead)
            .checksum_algorithm(ChecksumAlgorithm::Sha256)
            .checksum_sha256(&checksum)
            .send()
            .await
            .map_err(|error| {
                ArtifactStorageError::with_source(
                    ArtifactStorageErrorKind::UploadUnavailable,
                    error,
                )
            })?;

        if output
            .checksum_sha256()
            .is_some_and(|returned| returned != checksum)
        {
            return Err(ArtifactStorageError::new(
                ArtifactStorageErrorKind::ChecksumMismatch,
            ));
        }

        Ok(StoredArtifact {
            sha256: artifact.sha256,
            byte_size: artifact.byte_size,
            storage_key,
        })
    }
}

#[async_trait]
impl VersionedArtifactStorage for SpacesArtifactStorage {
    async fn list_object_versions(
        &self,
        cursor: Option<&ObjectVersionCursor>,
        limit: u16,
    ) -> Result<ObjectVersionPage, ObjectVersionStorageError> {
        let output = self
            .client
            .list_object_versions()
            .bucket(&self.bucket)
            .prefix(PACKAGE_OBJECT_PREFIX)
            .max_keys(i32::from(limit))
            .set_key_marker(cursor.map(|item| item.key_marker.clone()))
            .set_version_id_marker(cursor.and_then(|item| item.version_id_marker.clone()))
            .send()
            .await
            .map_err(|error| {
                ObjectVersionStorageError::with_source(
                    ObjectVersionStorageErrorKind::ListUnavailable,
                    error,
                )
            })?;

        let versions = output
            .versions()
            .iter()
            .map(|version| {
                let last_modified = version
                    .last_modified()
                    .map(|timestamp| {
                        let nanos = i128::from(timestamp.secs()) * 1_000_000_000
                            + i128::from(timestamp.subsec_nanos());
                        OffsetDateTime::from_unix_timestamp_nanos(nanos)
                    })
                    .transpose()
                    .map_err(|error| {
                        ObjectVersionStorageError::with_source(
                            ObjectVersionStorageErrorKind::InvalidResponse,
                            error,
                        )
                    })?;
                Ok(StoredObjectVersion {
                    key: version.key().map(str::to_owned),
                    version_id: version.version_id().map(str::to_owned),
                    last_modified,
                })
            })
            .collect::<Result<Vec<_>, ObjectVersionStorageError>>()?;
        let next_cursor = if output.is_truncated() == Some(true) {
            let key_marker = output.next_key_marker().ok_or_else(|| {
                ObjectVersionStorageError::new(ObjectVersionStorageErrorKind::InvalidResponse)
            })?;
            Some(ObjectVersionCursor {
                key_marker: key_marker.to_owned(),
                version_id_marker: output.next_version_id_marker().map(str::to_owned),
            })
        } else {
            None
        };

        Ok(ObjectVersionPage {
            versions,
            next_cursor,
        })
    }

    async fn delete_object_version(
        &self,
        key: &str,
        version_id: &str,
    ) -> Result<(), ObjectVersionStorageError> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .version_id(version_id)
            .send()
            .await
            .map_err(|error| {
                ObjectVersionStorageError::with_source(
                    ObjectVersionStorageErrorKind::DeleteUnavailable,
                    error,
                )
            })?;
        Ok(())
    }
}

fn storage_key(artifact: &ArtifactUpload) -> String {
    format!(
        "packages/{}/{}/{}/{}.ruxpkg",
        artifact.namespace.normalized(),
        artifact.package.normalized(),
        artifact.version.as_str(),
        checksum_hex(artifact.sha256.as_bytes())
    )
}

fn checksum_hex(checksum: &[u8; 32]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in checksum {
        write!(&mut encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use aws_config::BehaviorVersion;
    use aws_credential_types::Credentials;
    use axum::Router;
    use axum::body::{Body, Bytes};
    use axum::extract::{Request, State};
    use axum::http::{HeaderMap, Method, StatusCode, Uri};
    use axum::response::Response;
    use axum::routing::{get, put};
    use rux_application::{ArtifactSha256, VersionedArtifactStorage};
    use rux_domain::{IdentitySegment, SemanticVersion};
    use time::format_description::well_known::Rfc3339;

    use super::*;

    #[test]
    fn immutable_keys_use_normalized_identity_exact_version_and_checksum() {
        let artifact = ArtifactUpload {
            path: PathBuf::from("ignored.ruxpkg"),
            namespace: IdentitySegment::new("Rux_Tools").expect("valid namespace"),
            package: IdentitySegment::new("Example_Pkg").expect("valid package"),
            version: SemanticVersion::new("1.2.3-beta.1+native").expect("valid version"),
            sha256: ArtifactSha256::new([0xab; 32]),
            byte_size: 42,
        };

        assert_eq!(
            storage_key(&artifact),
            concat!(
                "packages/rux-tools/example-pkg/1.2.3-beta.1+native/",
                "abababababababababababababababababababababababababababababababab.ruxpkg"
            )
        );
    }

    #[derive(Clone, Debug)]
    struct CapturedRequest {
        method: Method,
        uri: Uri,
        headers: HeaderMap,
        body: Bytes,
    }

    #[tokio::test]
    async fn put_streams_exact_bytes_with_sha256_and_immutable_metadata() {
        let captured = Arc::new(Mutex::new(None));
        let app = Router::new()
            .route("/{*path}", put(capture_put))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener has an address")
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test server should run");
        });
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "test-access",
                "test-secret",
                None,
                None,
                "object-storage-test",
            ))
            .region(aws_config::Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .http_client(aws_smithy_http_client::Builder::new().build_http())
            .load()
            .await;
        let config = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build();
        let storage = SpacesArtifactStorage::new(S3Client::from_conf(config), "packages".into());
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let path = directory.path().join("artifact.ruxpkg");
        let bytes = b"immutable package bytes";
        std::fs::write(&path, bytes).expect("artifact should write");
        let digest = ArtifactSha256::new([
            0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee,
            0xff, 0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0,
            0xd0, 0xe0, 0xf0, 0x01,
        ]);

        let stored = storage
            .store(ArtifactUpload {
                path,
                namespace: IdentitySegment::new("Rux_Tools").expect("valid namespace"),
                package: IdentitySegment::new("Example_Pkg").expect("valid package"),
                version: SemanticVersion::new("1.2.3").expect("valid version"),
                sha256: digest,
                byte_size: bytes.len() as u64,
            })
            .await
            .expect("upload should succeed");

        server.abort();
        let request = captured
            .lock()
            .expect("capture should not be poisoned")
            .take()
            .expect("request should be captured");
        assert_eq!(request.body.as_ref(), bytes);
        assert_eq!(request.method, Method::PUT);
        assert_eq!(request.headers["content-length"], bytes.len().to_string());
        assert_eq!(request.headers["content-type"], ARTIFACT_CONTENT_TYPE);
        assert_eq!(request.headers["cache-control"], ARTIFACT_CACHE_CONTROL);
        assert_eq!(request.headers["x-amz-acl"], "public-read");
        assert_eq!(request.headers["x-amz-sdk-checksum-algorithm"], "SHA256");
        assert_eq!(
            request.headers["x-amz-checksum-sha256"],
            STANDARD.encode(digest.as_bytes())
        );
        assert_eq!(
            request.uri.path(),
            format!("/packages/{}", stored.storage_key)
        );
        assert_eq!(stored.sha256, digest);
        assert_eq!(stored.byte_size, bytes.len() as u64);
    }

    async fn capture_put(
        State(captured): State<Arc<Mutex<Option<CapturedRequest>>>>,
        request: Request,
    ) -> Response {
        let (parts, body) = request.into_parts();
        let body = axum::body::to_bytes(body, usize::MAX)
            .await
            .expect("request body should read");
        *captured.lock().expect("capture should not be poisoned") = Some(CapturedRequest {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            body,
        });
        Response::builder()
            .status(StatusCode::OK)
            .body(Body::empty())
            .expect("response should build")
    }

    #[tokio::test]
    async fn version_listing_preserves_markers_and_exact_delete_targets_one_version() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/{*path}", get(version_response).delete(version_response))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let endpoint = format!(
            "http://{}",
            listener.local_addr().expect("listener has an address")
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test server should run");
        });
        let shared = aws_config::defaults(BehaviorVersion::latest())
            .credentials_provider(Credentials::new(
                "test-access",
                "test-secret",
                None,
                None,
                "object-version-test",
            ))
            .region(aws_config::Region::new("us-east-1"))
            .endpoint_url(endpoint)
            .http_client(aws_smithy_http_client::Builder::new().build_http())
            .load()
            .await;
        let config = aws_sdk_s3::config::Builder::from(&shared)
            .force_path_style(true)
            .build();
        let storage = SpacesArtifactStorage::new(S3Client::from_conf(config), "packages".into());
        let cursor = ObjectVersionCursor {
            key_marker: "cursor-key".into(),
            version_id_marker: Some("cursor-version".into()),
        };

        let page = storage
            .list_object_versions(Some(&cursor), 123)
            .await
            .expect("versions should list");
        storage
            .delete_object_version(
                "packages/rux/example/1.0.0/checksum.ruxpkg",
                "object-version",
            )
            .await
            .expect("exact version should delete");
        server.abort();

        assert_eq!(page.versions.len(), 1);
        assert_eq!(
            page.versions[0],
            StoredObjectVersion {
                key: Some("packages/rux/example/1.0.0/checksum.ruxpkg".into()),
                version_id: Some("object-version".into()),
                last_modified: Some(
                    OffsetDateTime::parse("2025-08-01T12:20:00Z", &Rfc3339)
                        .expect("fixture timestamp should be valid")
                ),
            }
        );
        assert_eq!(
            page.next_cursor,
            Some(ObjectVersionCursor {
                key_marker: "next-key".into(),
                version_id_marker: Some("next-version".into()),
            })
        );

        let requests = captured.lock().expect("requests should not be poisoned");
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, Method::GET);
        let list_query = requests[0].uri.query().expect("list query should exist");
        assert!(list_query.split('&').any(|item| item == "versions"));
        assert!(list_query.contains("prefix=packages%2F"));
        assert!(list_query.contains("max-keys=123"));
        assert!(list_query.contains("key-marker=cursor-key"));
        assert!(list_query.contains("version-id-marker=cursor-version"));
        assert_eq!(requests[1].method, Method::DELETE);
        assert_eq!(
            requests[1].uri.path(),
            "/packages/packages/rux/example/1.0.0/checksum.ruxpkg"
        );
        assert!(
            requests[1]
                .uri
                .query()
                .expect("delete query should exist")
                .split('&')
                .any(|item| item == "versionId=object-version")
        );
    }

    async fn version_response(
        State(captured): State<Arc<Mutex<Vec<CapturedRequest>>>>,
        request: Request,
    ) -> Response {
        let (parts, body) = request.into_parts();
        let method = parts.method.clone();
        let body = axum::body::to_bytes(body, usize::MAX)
            .await
            .expect("request body should read");
        captured
            .lock()
            .expect("requests should not be poisoned")
            .push(CapturedRequest {
                method: parts.method,
                uri: parts.uri,
                headers: parts.headers,
                body,
            });
        let response_body = if method == Method::GET {
            Body::from(concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
                "<ListVersionsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
                "<Name>packages</Name><Prefix>packages/</Prefix>",
                "<KeyMarker>cursor-key</KeyMarker>",
                "<VersionIdMarker>cursor-version</VersionIdMarker>",
                "<NextKeyMarker>next-key</NextKeyMarker>",
                "<NextVersionIdMarker>next-version</NextVersionIdMarker>",
                "<MaxKeys>123</MaxKeys><IsTruncated>true</IsTruncated>",
                "<Version>",
                "<Key>packages/rux/example/1.0.0/checksum.ruxpkg</Key>",
                "<VersionId>object-version</VersionId><IsLatest>true</IsLatest>",
                "<LastModified>2025-08-01T12:20:00Z</LastModified>",
                "<ETag>&quot;etag&quot;</ETag><Size>42</Size><StorageClass>STANDARD</StorageClass>",
                "</Version></ListVersionsResult>"
            ))
        } else {
            Body::empty()
        };
        Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/xml")
            .body(response_body)
            .expect("response should build")
    }
}
