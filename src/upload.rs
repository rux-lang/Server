use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::Multipart;
use axum::extract::multipart::{Field, MultipartError};
use http::StatusCode;
use rux_application::ArtifactSha256;
use rux_artifact::{ARTIFACT_MAX_BYTES, ArtifactError, ArtifactInspection, inspect_artifact};
use rux_manifest::MANIFEST_MAX_BYTES;
use sha2::{Digest, Sha256};
use tempfile::{Builder, NamedTempFile};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinError;

/// Maximum size of the complete multipart publication request, including framing.
pub const UPLOAD_REQUEST_MAX_BYTES: usize = 6 * 1024 * 1024;
/// Default process-wide allowance for package bytes retained on temporary storage.
pub const DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES: u64 = 100 * 1024 * 1024;

const DEFAULT_UPLOAD_TEMPORARY_DIRECTORY: &str = "rux-server-uploads";
const MANIFEST_PART_NAME: &str = "manifest";
const PACKAGE_PART_NAME: &str = "package";

/// Receives bounded publication forms into process-local staging storage.
#[derive(Clone, Debug)]
pub struct UploadReceiver {
    inner: Arc<UploadReceiverInner>,
}

#[derive(Debug)]
struct UploadReceiverInner {
    directory: PathBuf,
    quota: Arc<TemporaryQuota>,
}

impl UploadReceiver {
    /// Creates a receiver using an explicit staging directory and aggregate byte allowance.
    ///
    /// # Errors
    ///
    /// Returns an error when the allowance cannot hold one maximum-size artifact, or when the
    /// staging directory cannot be created or is not a directory.
    pub fn new(directory: impl AsRef<Path>, temporary_capacity_bytes: u64) -> io::Result<Self> {
        if temporary_capacity_bytes < ARTIFACT_MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("temporary upload capacity must be at least {ARTIFACT_MAX_BYTES} bytes"),
            ));
        }

        let directory = directory.as_ref().to_path_buf();
        std::fs::create_dir_all(&directory)?;
        if !directory.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "temporary upload path is not a directory",
            ));
        }

        Ok(Self {
            inner: Arc::new(UploadReceiverInner {
                directory,
                quota: Arc::new(TemporaryQuota::new(temporary_capacity_bytes)),
            }),
        })
    }

    /// Creates a receiver below the operating system's temporary directory with a 100 MiB quota.
    ///
    /// # Errors
    ///
    /// Returns an error when the staging directory cannot be created.
    pub fn with_defaults() -> io::Result<Self> {
        Self::new(
            std::env::temp_dir().join(DEFAULT_UPLOAD_TEMPORARY_DIRECTORY),
            DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES,
        )
    }

    /// Returns the configured process-wide temporary byte allowance.
    #[must_use]
    pub fn temporary_capacity_bytes(&self) -> u64 {
        self.inner.quota.capacity
    }

    /// Returns package bytes currently reserved by active upload operations and results.
    #[must_use]
    pub fn temporary_bytes_in_use(&self) -> u64 {
        self.inner.quota.used()
    }

    /// Streams exactly one `manifest` and one `package` multipart field into a staged upload.
    ///
    /// Parts may arrive in either order. Caller-provided filenames and content types do not
    /// influence storage or validation.
    ///
    /// # Errors
    ///
    /// Returns a stable upload error for malformed shape, request or part limits, exhausted
    /// temporary capacity, or staging I/O failures.
    pub async fn receive(&self, mut multipart: Multipart) -> Result<ReceivedUpload, UploadError> {
        let mut manifest = None;
        let mut package = None;

        while let Some(field) = multipart.next_field().await.map_err(map_multipart_error)? {
            let Some(name) = field.name().map(str::to_owned) else {
                return Err(UploadError::new(UploadErrorKind::UnexpectedPart));
            };

            match name.as_str() {
                MANIFEST_PART_NAME => {
                    if manifest.is_some() {
                        return Err(UploadError::new(UploadErrorKind::DuplicateManifest));
                    }
                    manifest = Some(read_manifest(field).await?);
                }
                PACKAGE_PART_NAME => {
                    if package.is_some() {
                        return Err(UploadError::new(UploadErrorKind::DuplicatePackage));
                    }
                    package = Some(self.receive_package(field).await?);
                }
                _ => return Err(UploadError::new(UploadErrorKind::UnexpectedPart)),
            }
        }

        let manifest =
            manifest.ok_or_else(|| UploadError::new(UploadErrorKind::MissingManifest))?;
        let package = package.ok_or_else(|| UploadError::new(UploadErrorKind::MissingPackage))?;
        Ok(ReceivedUpload { manifest, package })
    }

    async fn receive_package(
        &self,
        mut field: Field<'_>,
    ) -> Result<TemporaryArtifact, UploadError> {
        let temporary_file = Builder::new()
            .prefix("rux-upload-")
            .suffix(".ruxpkg")
            .tempfile_in(&self.inner.directory)
            .map_err(|error| {
                UploadError::with_source(UploadErrorKind::TemporaryStorageUnavailable, error)
            })?;
        let writer = temporary_file.reopen().map_err(|error| {
            UploadError::with_source(UploadErrorKind::TemporaryStorageUnavailable, error)
        })?;
        let mut writer = tokio::fs::File::from_std(writer);
        let mut reservation = TemporaryReservation::new(self.inner.quota.clone());
        let mut byte_size = 0_u64;
        let mut hasher = Sha256::new();

        while let Some(chunk) = field.chunk().await.map_err(map_multipart_error)? {
            let chunk_size = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            let Some(new_size) = byte_size.checked_add(chunk_size) else {
                return Err(UploadError::new(UploadErrorKind::ArtifactTooLarge));
            };
            if new_size > ARTIFACT_MAX_BYTES {
                return Err(UploadError::new(UploadErrorKind::ArtifactTooLarge));
            }
            reservation.try_add(chunk_size)?;
            writer.write_all(&chunk).await.map_err(|error| {
                UploadError::with_source(UploadErrorKind::TemporaryStorageUnavailable, error)
            })?;
            hasher.update(&chunk);
            byte_size = new_size;
        }

        writer.flush().await.map_err(|error| {
            UploadError::with_source(UploadErrorKind::TemporaryStorageUnavailable, error)
        })?;
        drop(writer);

        Ok(TemporaryArtifact {
            file: temporary_file,
            byte_size,
            sha256: ArtifactSha256::new(hasher.finalize().into()),
            _reservation: reservation,
        })
    }
}

async fn read_manifest(mut field: Field<'_>) -> Result<Vec<u8>, UploadError> {
    let mut manifest = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(map_multipart_error)? {
        let Some(new_size) = manifest.len().checked_add(chunk.len()) else {
            return Err(UploadError::new(UploadErrorKind::ManifestTooLarge));
        };
        if new_size > MANIFEST_MAX_BYTES {
            return Err(UploadError::new(UploadErrorKind::ManifestTooLarge));
        }
        manifest.extend_from_slice(&chunk);
    }
    Ok(manifest)
}

fn map_multipart_error(error: MultipartError) -> UploadError {
    let kind = match error.status() {
        StatusCode::PAYLOAD_TOO_LARGE => UploadErrorKind::RequestTooLarge,
        status if status.is_server_error() => UploadErrorKind::RequestReadFailed,
        _ => UploadErrorKind::MalformedMultipart,
    };
    UploadError::with_source(kind, error)
}

/// A received publication form whose temporary artifact is deleted when dropped.
#[derive(Debug)]
pub struct ReceivedUpload {
    manifest: Vec<u8>,
    package: TemporaryArtifact,
}

impl ReceivedUpload {
    /// Returns the exact uploaded manifest bytes.
    #[must_use]
    pub fn manifest(&self) -> &[u8] {
        &self.manifest
    }

    /// Returns the staged package artifact and its transport metadata.
    #[must_use]
    pub const fn package(&self) -> &TemporaryArtifact {
        &self.package
    }

    /// Separates the manifest and owned package staging guard for later inspection.
    #[must_use]
    pub fn into_parts(self) -> (Vec<u8>, TemporaryArtifact) {
        (self.manifest, self.package)
    }

    /// Validates the staged artifact and its exact uploaded manifest on a blocking worker.
    ///
    /// The complete archive remains on temporary storage. Successful inspection transfers its
    /// staging guard into the returned value so later publication steps can continue using the
    /// same bytes and transport metadata.
    ///
    /// # Errors
    ///
    /// Returns a stable inspection error when the staged file cannot be reopened, the manifest or
    /// artifact contract is invalid, or the blocking worker cannot complete.
    pub async fn inspect(self) -> Result<InspectedUpload, InspectionError> {
        let (manifest, package) = self.into_parts();

        tokio::task::spawn_blocking(move || {
            let reader = package
                .reopen_for_inspection()
                .map_err(InspectionError::temporary_storage)?;
            let inspection =
                inspect_artifact(reader, &manifest).map_err(InspectionError::artifact)?;

            Ok(InspectedUpload {
                inspection,
                package,
            })
        })
        .await
        .map_err(InspectionError::worker)?
    }
}

/// A staged package artifact plus transport metadata and its temporary-capacity reservation.
#[derive(Debug)]
pub struct TemporaryArtifact {
    file: NamedTempFile,
    byte_size: u64,
    sha256: ArtifactSha256,
    _reservation: TemporaryReservation,
}

impl TemporaryArtifact {
    /// Returns the number of uploaded package bytes.
    #[must_use]
    pub const fn byte_size(&self) -> u64 {
        self.byte_size
    }

    /// Returns the SHA-256 digest computed while receiving the package.
    #[must_use]
    pub const fn sha256(&self) -> ArtifactSha256 {
        self.sha256
    }

    /// Returns the generated staging path for trusted internal adapters.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Opens a new standard file handle positioned at the start for synchronous inspection.
    ///
    /// Callers from asynchronous code should invoke this and artifact inspection on a blocking
    /// worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the staged file can no longer be reopened.
    pub fn reopen_for_inspection(&self) -> io::Result<File> {
        self.file.reopen()
    }
}

/// A validated publication upload retaining its temporary artifact and transport metadata.
#[derive(Debug)]
pub struct InspectedUpload {
    inspection: ArtifactInspection,
    package: TemporaryArtifact,
}

impl InspectedUpload {
    /// Returns the validated manifest, referenced text, and archive metrics.
    #[must_use]
    pub const fn inspection(&self) -> &ArtifactInspection {
        &self.inspection
    }

    /// Returns the staged package artifact and its transport metadata.
    #[must_use]
    pub const fn package(&self) -> &TemporaryArtifact {
        &self.package
    }

    /// Separates the validated metadata and owned package staging guard for publication.
    #[must_use]
    pub fn into_parts(self) -> (ArtifactInspection, TemporaryArtifact) {
        (self.inspection, self.package)
    }
}

/// Stable internal categories for staged upload inspection failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InspectionErrorKind {
    InvalidArtifact,
    TemporaryStorageUnavailable,
    InspectionUnavailable,
}

#[derive(Debug)]
enum InspectionErrorSource {
    Artifact(ArtifactError),
    TemporaryStorage(io::Error),
    Worker(JoinError),
}

/// A staged upload inspection failure retaining its safe underlying diagnostic.
#[derive(Debug)]
pub struct InspectionError {
    kind: InspectionErrorKind,
    source: InspectionErrorSource,
}

impl InspectionError {
    fn artifact(source: ArtifactError) -> Self {
        Self {
            kind: InspectionErrorKind::InvalidArtifact,
            source: InspectionErrorSource::Artifact(source),
        }
    }

    fn temporary_storage(source: io::Error) -> Self {
        Self {
            kind: InspectionErrorKind::TemporaryStorageUnavailable,
            source: InspectionErrorSource::TemporaryStorage(source),
        }
    }

    fn worker(source: JoinError) -> Self {
        Self {
            kind: InspectionErrorKind::InspectionUnavailable,
            source: InspectionErrorSource::Worker(source),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn kind(&self) -> InspectionErrorKind {
        self.kind
    }

    /// Returns the artifact diagnostic when validation rejected the upload.
    #[must_use]
    pub const fn artifact_error(&self) -> Option<&ArtifactError> {
        match &self.source {
            InspectionErrorSource::Artifact(error) => Some(error),
            InspectionErrorSource::TemporaryStorage(_) | InspectionErrorSource::Worker(_) => None,
        }
    }
}

impl fmt::Display for InspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "upload inspection failed: {:?}", self.kind)
    }
}

impl Error for InspectionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(match &self.source {
            InspectionErrorSource::Artifact(error) => error,
            InspectionErrorSource::TemporaryStorage(error) => error,
            InspectionErrorSource::Worker(error) => error,
        })
    }
}

/// Stable internal categories for upload reception failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadErrorKind {
    MalformedMultipart,
    RequestReadFailed,
    RequestTooLarge,
    UnexpectedPart,
    DuplicateManifest,
    DuplicatePackage,
    MissingManifest,
    MissingPackage,
    ManifestTooLarge,
    ArtifactTooLarge,
    TemporaryCapacityExceeded,
    TemporaryStorageUnavailable,
}

/// A bounded upload reception failure.
#[derive(Debug)]
pub struct UploadError {
    kind: UploadErrorKind,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl UploadError {
    fn new(kind: UploadErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn with_source(kind: UploadErrorKind, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            source: Some(Box::new(source)),
        }
    }

    /// Returns the stable failure category.
    #[must_use]
    pub const fn kind(&self) -> UploadErrorKind {
        self.kind
    }
}

impl fmt::Display for UploadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "upload reception failed: {:?}", self.kind)
    }
}

impl Error for UploadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

#[derive(Debug)]
struct TemporaryQuota {
    capacity: u64,
    used: AtomicU64,
}

impl TemporaryQuota {
    const fn new(capacity: u64) -> Self {
        Self {
            capacity,
            used: AtomicU64::new(0),
        }
    }

    fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    fn try_add(&self, bytes: u64) -> bool {
        self.used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|new_used| *new_used <= self.capacity)
            })
            .is_ok()
    }

    fn release(&self, bytes: u64) {
        let previous = self.used.fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(previous >= bytes, "temporary upload quota underflow");
    }
}

#[derive(Debug)]
struct TemporaryReservation {
    quota: Arc<TemporaryQuota>,
    bytes: u64,
}

impl TemporaryReservation {
    const fn new(quota: Arc<TemporaryQuota>) -> Self {
        Self { quota, bytes: 0 }
    }

    fn try_add(&mut self, bytes: u64) -> Result<(), UploadError> {
        if !self.quota.try_add(bytes) {
            return Err(UploadError::new(UploadErrorKind::TemporaryCapacityExceeded));
        }
        self.bytes += bytes;
        Ok(())
    }
}

impl Drop for TemporaryReservation {
    fn drop(&mut self) {
        self.quota.release(self.bytes);
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::io::{Cursor, Read, Write};
    use std::sync::Mutex;

    use axum::Router;
    use axum::body::{Body, Bytes, HttpBody};
    use axum::extract::{DefaultBodyLimit, State};
    use axum::http::Request;
    use axum::routing::post;
    use futures_util::stream::{self, StreamExt};
    use rux_artifact::ArtifactErrorCode;
    use rux_manifest::{ManifestErrorCode, ManifestKind};
    use tower::ServiceExt;
    use tower_http::limit::RequestBodyLimitLayer;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::*;

    const BOUNDARY: &str = "rux-upload-boundary";
    const VALID_MANIFEST: &str = r#"[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Namespace = "Rux"
Name = "Example"
Version = "1.2.3"
Type = "Source"
License = "MIT"
LicenseFile = "LICENSE"
ReadmeFile = "README.md"
"#;
    const LOCAL_ONLY_MANIFEST: &str = r#"[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Name = "Example"
Version = "1.2.3"
Type = "Source"
"#;

    type CapturedUpload = Arc<Mutex<Option<Result<ReceivedUpload, UploadError>>>>;

    #[derive(Clone)]
    struct CaptureState {
        receiver: UploadReceiver,
        result: CapturedUpload,
    }

    struct Part<'part> {
        name: Option<&'part str>,
        file_name: Option<&'part str>,
        content_type: Option<&'part str>,
        bytes: &'part [u8],
    }

    #[tokio::test]
    async fn parts_stream_in_either_order_and_transport_metadata_is_exact() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let package = b"package bytes";
        let manifest = b"[Manifest]\nVersion = 1\n";
        let body = multipart_body(&[
            Part {
                name: Some(PACKAGE_PART_NAME),
                file_name: Some("../../caller-name.ruxpkg"),
                content_type: Some("application/octet-stream"),
                bytes: package,
            },
            Part {
                name: Some(MANIFEST_PART_NAME),
                file_name: Some("not-rux.toml"),
                content_type: Some("text/plain"),
                bytes: manifest,
            },
        ]);

        let (status, result) = submit_bytes(receiver.clone(), body, true).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let upload = result
            .expect("handler should run")
            .expect("upload should succeed");
        assert_eq!(upload.manifest(), manifest);
        assert_eq!(upload.package().byte_size(), package.len() as u64);
        assert_eq!(
            upload.package().sha256(),
            ArtifactSha256::new(Sha256::digest(package).into())
        );
        let mut stored = Vec::new();
        upload
            .package()
            .reopen_for_inspection()
            .expect("artifact should reopen")
            .read_to_end(&mut stored)
            .expect("artifact should be readable");
        assert_eq!(stored, package);
        assert_eq!(receiver.temporary_bytes_in_use(), package.len() as u64);
        assert_eq!(directory_entries(&directory), 1);

        drop(upload);
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[tokio::test]
    async fn received_upload_inspects_from_disk_and_retains_transport_metadata() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let source = b"first\nsecond";
        let readme_file = b"# Example\n";
        let license = b"Example license";
        let entries: [(&str, &[u8]); 3] = [
            ("Src/Main.rux", source),
            ("README.md", readme_file),
            ("LICENSE", license),
        ];
        let package = artifact_package(VALID_MANIFEST, &entries);
        let expected_size = u64::try_from(package.len()).expect("fixture size should fit u64");
        let expected_sha256 = ArtifactSha256::new(Sha256::digest(&package).into());
        let body = multipart_body(&[
            part(MANIFEST_PART_NAME, VALID_MANIFEST.as_bytes()),
            part(PACKAGE_PART_NAME, &package),
        ]);

        let (_, result) = submit_bytes(receiver.clone(), body, true).await;
        let upload = result
            .expect("handler should run")
            .expect("upload should succeed");
        let inspected = upload.inspect().await.expect("artifact should inspect");

        let ManifestKind::Package(manifest) = inspected.inspection().manifest().kind() else {
            panic!("publication inspection should return a package manifest");
        };
        assert_eq!(
            manifest.namespace().map(ToString::to_string).as_deref(),
            Some("Rux")
        );
        assert_eq!(manifest.name().to_string(), "Example");
        assert_eq!(inspected.inspection().file_count(), 4);
        assert_eq!(inspected.inspection().source_file_count(), 1);
        assert_eq!(inspected.inspection().source_line_count(), 2);
        assert_eq!(inspected.inspection().readme_file(), Some("# Example\n"));
        assert_eq!(
            inspected.inspection().license_file(),
            Some("Example license")
        );
        assert_eq!(inspected.package().byte_size(), expected_size);
        assert_eq!(inspected.package().sha256(), expected_sha256);
        assert_eq!(receiver.temporary_bytes_in_use(), expected_size);
        assert_eq!(directory_entries(&directory), 1);

        drop(inspected);
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[tokio::test]
    async fn inspection_preserves_artifact_diagnostics_and_releases_staging() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let source = b"source";
        let entries: [(&str, &[u8]); 1] = [("Src/Main.rux", source)];
        let package = artifact_package(VALID_MANIFEST, &entries);
        let body = multipart_body(&[
            part(MANIFEST_PART_NAME, LOCAL_ONLY_MANIFEST.as_bytes()),
            part(PACKAGE_PART_NAME, &package),
        ]);
        let (_, result) = submit_bytes(receiver.clone(), body, true).await;
        let upload = result
            .expect("handler should run")
            .expect("upload should succeed");

        let error = upload
            .inspect()
            .await
            .expect_err("different embedded manifest should fail");
        assert_eq!(error.kind(), InspectionErrorKind::InvalidArtifact);
        assert_eq!(
            error
                .artifact_error()
                .expect("artifact diagnostic should be retained")
                .code(),
            ArtifactErrorCode::ManifestMismatch
        );
        assert!(error.source().is_some());
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[tokio::test]
    async fn inspection_preserves_publication_manifest_diagnostics() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let source = b"source";
        let entries: [(&str, &[u8]); 1] = [("Src/Main.rux", source)];
        let package = artifact_package(LOCAL_ONLY_MANIFEST, &entries);
        let body = multipart_body(&[
            part(MANIFEST_PART_NAME, LOCAL_ONLY_MANIFEST.as_bytes()),
            part(PACKAGE_PART_NAME, &package),
        ]);
        let (_, result) = submit_bytes(receiver.clone(), body, true).await;
        let upload = result
            .expect("handler should run")
            .expect("upload should succeed");

        let error = upload
            .inspect()
            .await
            .expect_err("local-only manifest should not publish");
        let artifact_error = error
            .artifact_error()
            .expect("artifact diagnostic should be retained");
        assert_eq!(artifact_error.code(), ArtifactErrorCode::InvalidManifest);
        assert_eq!(
            artifact_error
                .manifest_errors()
                .expect("manifest diagnostics should be retained")
                .as_slice()[0]
                .code(),
            ManifestErrorCode::MissingField
        );
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[test]
    fn inspection_operational_errors_are_classified_without_artifact_diagnostics() {
        let error = InspectionError::temporary_storage(io::Error::new(
            io::ErrorKind::NotFound,
            "staged artifact disappeared",
        ));

        assert_eq!(
            error.kind(),
            InspectionErrorKind::TemporaryStorageUnavailable
        );
        assert!(error.artifact_error().is_none());
        assert!(error.source().is_some());
    }

    #[tokio::test]
    async fn inclusive_manifest_and_artifact_limits_are_accepted() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let manifest = vec![b'm'; MANIFEST_MAX_BYTES];
        let package = vec![b'p'; usize::try_from(ARTIFACT_MAX_BYTES).unwrap()];
        let body = multipart_body(&[
            part(MANIFEST_PART_NAME, &manifest),
            part(PACKAGE_PART_NAME, &package),
        ]);
        assert!(body.len() < UPLOAD_REQUEST_MAX_BYTES);

        let (status, result) = submit_bytes(receiver, body, true).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let upload = result
            .expect("handler should run")
            .expect("upload should succeed");
        assert_eq!(upload.manifest().len(), MANIFEST_MAX_BYTES);
        assert_eq!(upload.package().byte_size(), ARTIFACT_MAX_BYTES);
    }

    #[tokio::test]
    async fn individual_part_overflows_are_rejected_and_cleaned_up() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let oversized_manifest = vec![b'm'; MANIFEST_MAX_BYTES + 1];
        let small_package = b"p";
        let manifest_body = multipart_body(&[
            part(MANIFEST_PART_NAME, &oversized_manifest),
            part(PACKAGE_PART_NAME, small_package),
        ]);
        assert_error(
            receiver.clone(),
            manifest_body,
            UploadErrorKind::ManifestTooLarge,
        )
        .await;

        let manifest = b"m";
        let oversized_package = vec![b'p'; usize::try_from(ARTIFACT_MAX_BYTES).unwrap() + 1];
        let package_body = multipart_body(&[
            part(MANIFEST_PART_NAME, manifest),
            part(PACKAGE_PART_NAME, &oversized_package),
        ]);
        assert_error(
            receiver.clone(),
            package_body,
            UploadErrorKind::ArtifactTooLarge,
        )
        .await;

        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[tokio::test]
    async fn multipart_shape_is_exact() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let manifest = b"m";
        let package = b"p";

        let cases = [
            (
                multipart_body(&[part(PACKAGE_PART_NAME, package)]),
                UploadErrorKind::MissingManifest,
            ),
            (
                multipart_body(&[part(MANIFEST_PART_NAME, manifest)]),
                UploadErrorKind::MissingPackage,
            ),
            (
                multipart_body(&[
                    part(MANIFEST_PART_NAME, manifest),
                    part(MANIFEST_PART_NAME, manifest),
                ]),
                UploadErrorKind::DuplicateManifest,
            ),
            (
                multipart_body(&[
                    part(PACKAGE_PART_NAME, package),
                    part(PACKAGE_PART_NAME, package),
                ]),
                UploadErrorKind::DuplicatePackage,
            ),
            (
                multipart_body(&[
                    part(MANIFEST_PART_NAME, manifest),
                    part("metadata", manifest),
                ]),
                UploadErrorKind::UnexpectedPart,
            ),
            (
                multipart_body(&[Part {
                    name: None,
                    file_name: None,
                    content_type: None,
                    bytes: manifest,
                }]),
                UploadErrorKind::UnexpectedPart,
            ),
        ];

        for (body, expected) in cases {
            assert_error(receiver.clone(), body, expected).await;
        }
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[tokio::test]
    async fn malformed_multipart_is_distinct_from_shape_errors() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let body = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\nincomplete"
        )
        .into_bytes();

        assert_error(receiver, body, UploadErrorKind::MalformedMultipart).await;
    }

    #[tokio::test]
    async fn aggregate_quota_is_held_until_the_artifact_is_dropped() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, ARTIFACT_MAX_BYTES);
        let manifest = b"m";
        let maximum_package = vec![b'p'; usize::try_from(ARTIFACT_MAX_BYTES).unwrap()];
        let first_body = multipart_body(&[
            part(MANIFEST_PART_NAME, manifest),
            part(PACKAGE_PART_NAME, &maximum_package),
        ]);
        let (_, result) = submit_bytes(receiver.clone(), first_body, true).await;
        let first = result
            .expect("handler should run")
            .expect("first upload should fit");
        assert_eq!(receiver.temporary_bytes_in_use(), ARTIFACT_MAX_BYTES);

        let second_body = multipart_body(&[
            part(MANIFEST_PART_NAME, manifest),
            part(PACKAGE_PART_NAME, b"x"),
        ]);
        assert_error(
            receiver.clone(),
            second_body,
            UploadErrorKind::TemporaryCapacityExceeded,
        )
        .await;

        drop(first);
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        let retry_body = multipart_body(&[
            part(MANIFEST_PART_NAME, manifest),
            part(PACKAGE_PART_NAME, b"x"),
        ]);
        let (_, result) = submit_bytes(receiver, retry_body, true).await;
        result
            .expect("handler should run")
            .expect("released quota should be reusable");
    }

    #[tokio::test]
    async fn unavailable_staging_directory_is_reported_without_retaining_quota() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, ARTIFACT_MAX_BYTES);
        directory
            .close()
            .expect("temporary directory should be removable");
        let body = multipart_body(&[
            part(PACKAGE_PART_NAME, b"package"),
            part(MANIFEST_PART_NAME, b"manifest"),
        ]);

        assert_error(
            receiver.clone(),
            body,
            UploadErrorKind::TemporaryStorageUnavailable,
        )
        .await;
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
    }

    #[tokio::test]
    async fn fixed_and_streamed_requests_are_hard_limited_to_six_mebibytes() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let oversized = vec![b'x'; UPLOAD_REQUEST_MAX_BYTES + 1];

        let (fixed_status, fixed_result) =
            submit_bytes(receiver.clone(), oversized.clone(), true).await;
        assert_eq!(fixed_status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(
            fixed_result.is_none(),
            "known length should be rejected before extraction"
        );

        let stream = stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(oversized)) });
        let (streamed_status, streamed_result) =
            submit_body(receiver.clone(), Body::from_stream(stream), false).await;
        assert_eq!(streamed_status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            streamed_result
                .expect("unknown-length request should reach the handler")
                .expect_err("streamed request should exceed the limit")
                .kind(),
            UploadErrorKind::RequestTooLarge
        );
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[tokio::test]
    async fn cancellation_deletes_partial_artifacts_and_releases_quota() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let receiver = receiver(&directory, DEFAULT_UPLOAD_TEMPORARY_CAPACITY_BYTES);
        let prefix = format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"package\"\r\n\r\npackage\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"manifest\"\r\n\r\n"
        );
        let stream = stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(prefix)) })
            .chain(stream::pending());
        let (app, _) = capture_app(receiver.clone());
        let request = request(Body::from_stream(stream), false);
        let task = tokio::spawn(async move { app.oneshot(request).await });

        for _ in 0..10_000 {
            if receiver.temporary_bytes_in_use() > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(receiver.temporary_bytes_in_use(), 7);
        assert_eq!(directory_entries(&directory), 1);

        task.abort();
        let error = task.await.expect_err("request task should be cancelled");
        assert!(error.is_cancelled());
        assert_eq!(receiver.temporary_bytes_in_use(), 0);
        assert_eq!(directory_entries(&directory), 0);
    }

    #[test]
    fn receiver_configuration_requires_space_for_one_artifact() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let error = UploadReceiver::new(directory.path(), ARTIFACT_MAX_BYTES - 1)
            .expect_err("undersized quota should fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let receiver = receiver(&directory, ARTIFACT_MAX_BYTES);
        assert_eq!(receiver.temporary_capacity_bytes(), ARTIFACT_MAX_BYTES);
    }

    async fn capture(State(state): State<CaptureState>, multipart: Multipart) -> StatusCode {
        let result = state.receiver.receive(multipart).await;
        let status = match result.as_ref().map_err(UploadError::kind) {
            Ok(_) => StatusCode::NO_CONTENT,
            Err(UploadErrorKind::RequestTooLarge) => StatusCode::PAYLOAD_TOO_LARGE,
            Err(_) => StatusCode::BAD_REQUEST,
        };
        *state.result.lock().expect("capture should not be poisoned") = Some(result);
        status
    }

    fn capture_app(receiver: UploadReceiver) -> (Router, CapturedUpload) {
        let result = Arc::new(Mutex::new(None));
        let state = CaptureState {
            receiver,
            result: result.clone(),
        };
        let app = Router::new()
            .route("/upload", post(capture))
            .with_state(state)
            .layer(DefaultBodyLimit::disable())
            .layer(RequestBodyLimitLayer::new(UPLOAD_REQUEST_MAX_BYTES));
        (app, result)
    }

    async fn submit_bytes(
        receiver: UploadReceiver,
        bytes: Vec<u8>,
        content_length: bool,
    ) -> (StatusCode, Option<Result<ReceivedUpload, UploadError>>) {
        submit_body(receiver, Body::from(bytes), content_length).await
    }

    async fn submit_body(
        receiver: UploadReceiver,
        body: Body,
        content_length: bool,
    ) -> (StatusCode, Option<Result<ReceivedUpload, UploadError>>) {
        let length = body.size_hint().exact();
        let (app, captured) = capture_app(receiver);
        let response = app
            .oneshot(request_with_length(
                body,
                content_length.then_some(length).flatten(),
            ))
            .await
            .expect("router should respond");
        let result = captured
            .lock()
            .expect("capture should not be poisoned")
            .take();
        (response.status(), result)
    }

    fn request(body: Body, content_length: bool) -> Request<Body> {
        let length = body.size_hint().exact();
        request_with_length(body, content_length.then_some(length).flatten())
    }

    fn request_with_length(body: Body, content_length: Option<u64>) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri("/upload").header(
            http::header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        );
        if let Some(content_length) = content_length {
            builder = builder.header(http::header::CONTENT_LENGTH, content_length);
        }
        builder.body(body).expect("request should build")
    }

    async fn assert_error(receiver: UploadReceiver, body: Vec<u8>, expected: UploadErrorKind) {
        let (status, result) = submit_bytes(receiver, body, true).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            result
                .expect("handler should run")
                .expect_err("upload should fail")
                .kind(),
            expected
        );
    }

    fn receiver(directory: &tempfile::TempDir, capacity: u64) -> UploadReceiver {
        UploadReceiver::new(directory.path(), capacity).expect("receiver should be configured")
    }

    fn part<'part>(name: &'part str, bytes: &'part [u8]) -> Part<'part> {
        Part {
            name: Some(name),
            file_name: None,
            content_type: None,
            bytes,
        }
    }

    fn multipart_body(parts: &[Part<'_>]) -> Vec<u8> {
        let mut body = Vec::new();
        for part in parts {
            body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
            body.extend_from_slice(b"Content-Disposition: form-data");
            if let Some(name) = part.name {
                body.extend_from_slice(format!("; name=\"{name}\"").as_bytes());
            }
            if let Some(file_name) = part.file_name {
                body.extend_from_slice(format!("; filename=\"{file_name}\"").as_bytes());
            }
            body.extend_from_slice(b"\r\n");
            if let Some(content_type) = part.content_type {
                body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
            }
            body.extend_from_slice(b"\r\n");
            body.extend_from_slice(part.bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        body
    }

    fn artifact_package(manifest: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer
            .start_file("Rux.toml", options)
            .expect("manifest entry should start");
        writer
            .write_all(manifest.as_bytes())
            .expect("manifest entry should write");
        for (name, bytes) in entries {
            writer
                .start_file(*name, options)
                .expect("fixture entry should start");
            writer.write_all(bytes).expect("fixture entry should write");
        }
        writer
            .finish()
            .expect("fixture archive should finish")
            .into_inner()
    }

    fn directory_entries(directory: &tempfile::TempDir) -> usize {
        std::fs::read_dir(directory.path())
            .expect("temporary directory should be readable")
            .count()
    }
}
