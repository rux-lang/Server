#![doc = "Validation for ZIP-based Rux package artifacts."]

mod error;
mod inspector;
mod model;
mod path;

pub use error::{ArtifactError, ArtifactErrorCode};
pub use inspector::inspect_artifact;
pub use model::ArtifactInspection;

/// Maximum size of a complete `.ruxpkg` file.
pub const ARTIFACT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Maximum combined uncompressed size of all regular files.
pub const ARTIFACT_MAX_EXPANDED_BYTES: u64 = 10 * 1024 * 1024;
/// Maximum number of central-directory entries, including directories.
pub const ARTIFACT_MAX_ENTRIES: usize = 1_024;
/// Maximum uncompressed size of one regular file.
pub const ARTIFACT_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum uncompressed size of one Rux source file.
pub const ARTIFACT_MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;
/// Maximum size of a manifest-referenced README or license file.
pub const ARTIFACT_MAX_TEXT_BYTES: u64 = 1024 * 1024;
/// Maximum UTF-8 byte length of an archive entry path.
pub const ARTIFACT_MAX_PATH_BYTES: usize = 2_048;
