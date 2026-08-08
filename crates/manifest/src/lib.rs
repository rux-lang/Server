#![doc = "Versioned Rux.toml parsing and validation."]

mod error;
mod model;
mod parser;

pub use error::{ManifestError, ManifestErrorCode, ManifestErrors, SourcePosition, SourceSpan};
pub use model::{
    BuildConfiguration, BuildMode, DefineValue, Dependency, DependencySource,
    EffectiveBuildConfiguration, Manifest, ManifestHeader, ManifestKind, ManifestPath,
    Optimization, PackageManifest, PackageType, WebUrl, WorkspaceManifest,
};
pub use parser::{ValidationProfile, parse_manifest, parse_manifest_with_profile};
pub use rux_domain::MANIFEST_VERSION;

/// Earliest Rux compiler release that understands manifest schema v1.
pub const MANIFEST_MIN_RUX_VERSION: &str = "0.4.0";

pub const MANIFEST_MAX_BYTES: usize = 65_536;
pub const MANIFEST_MAX_DEPENDENCIES: usize = 256;
pub const MANIFEST_MAX_WORKSPACE_PACKAGES: usize = 256;
pub const MANIFEST_MAX_DEFINES: usize = 128;
pub const MANIFEST_MAX_AUTHORS: usize = 32;
pub const MANIFEST_MAX_KEYWORDS: usize = 32;
pub const MANIFEST_MAX_DESCRIPTION_BYTES: usize = 2_048;
pub const MANIFEST_MAX_AUTHOR_BYTES: usize = 256;
pub const MANIFEST_MAX_URL_OR_PATH_BYTES: usize = 2_048;
pub const MANIFEST_MAX_LICENSE_OR_RANGE_BYTES: usize = 512;
pub const MANIFEST_MAX_SEMANTIC_VERSION_BYTES: usize = 256;
pub const MANIFEST_MAX_DEFINE_STRING_BYTES: usize = 1_024;
pub const MANIFEST_MAX_KEYWORD_OR_DEFINE_NAME_BYTES: usize = 64;
