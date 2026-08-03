#![doc = "Core domain rules for the Rux package registry."]

mod identity;
mod version;

pub use identity::{IDENTITY_SEGMENT_MAX_LENGTH, IdentitySegment, IdentitySegmentError};
pub use version::{
    SemanticVersion, SemanticVersionError, VERSION_RANGE_MAX_COMPARATORS, VersionRange,
    VersionRangeError,
};

/// The first public registry API version.
pub const API_VERSION: &str = "v1";

/// The first supported Rux manifest schema version.
pub const MANIFEST_VERSION: u16 = 1;
