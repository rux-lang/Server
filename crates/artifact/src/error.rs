use std::error::Error;
use std::fmt;

use rux_manifest::ManifestErrors;

/// Stable machine-readable categories for artifact failures.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ArtifactErrorCode {
    Io,
    InvalidZip,
    ArtifactTooLarge,
    TooManyEntries,
    InvalidEntryPath,
    PathCollision,
    UnsupportedEntryType,
    UnsupportedCompression,
    EncryptedEntry,
    FileTooLarge,
    SourceTooLarge,
    TextTooLarge,
    ExpandedSizeTooLarge,
    MissingManifest,
    ManifestMismatch,
    InvalidManifest,
    MissingSource,
    InvalidUtf8Source,
    MissingReferencedFile,
    InvalidUtf8Text,
}

impl ArtifactErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::InvalidZip => "invalid_zip",
            Self::ArtifactTooLarge => "artifact_too_large",
            Self::TooManyEntries => "too_many_entries",
            Self::InvalidEntryPath => "invalid_entry_path",
            Self::PathCollision => "path_collision",
            Self::UnsupportedEntryType => "unsupported_entry_type",
            Self::UnsupportedCompression => "unsupported_compression",
            Self::EncryptedEntry => "encrypted_entry",
            Self::FileTooLarge => "file_too_large",
            Self::SourceTooLarge => "source_too_large",
            Self::TextTooLarge => "text_too_large",
            Self::ExpandedSizeTooLarge => "expanded_size_too_large",
            Self::MissingManifest => "missing_manifest",
            Self::ManifestMismatch => "manifest_mismatch",
            Self::InvalidManifest => "invalid_manifest",
            Self::MissingSource => "missing_source",
            Self::InvalidUtf8Source => "invalid_utf8_source",
            Self::MissingReferencedFile => "missing_referenced_file",
            Self::InvalidUtf8Text => "invalid_utf8_text",
        }
    }
}

/// One artifact validation failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactError {
    code: ArtifactErrorCode,
    entry: Option<String>,
    message: String,
    manifest_errors: Option<ManifestErrors>,
}

impl ArtifactError {
    pub(crate) fn new(
        code: ArtifactErrorCode,
        entry: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            entry,
            message: message.into(),
            manifest_errors: None,
        }
    }

    pub(crate) fn manifest(errors: ManifestErrors) -> Self {
        Self {
            code: ArtifactErrorCode::InvalidManifest,
            entry: Some("Rux.toml".to_owned()),
            message: errors.to_string(),
            manifest_errors: Some(errors),
        }
    }

    #[must_use]
    pub const fn code(&self) -> ArtifactErrorCode {
        self.code
    }

    #[must_use]
    pub fn entry(&self) -> Option<&str> {
        self.entry.as_deref()
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn manifest_errors(&self) -> Option<&ManifestErrors> {
        self.manifest_errors.as_ref()
    }
}

impl fmt::Display for ArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(entry) = &self.entry {
            write!(
                formatter,
                "{} for {entry}: {}",
                self.code.as_str(),
                self.message
            )
        } else {
            write!(formatter, "{}: {}", self.code.as_str(), self.message)
        }
    }
}

impl Error for ArtifactError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.manifest_errors
            .as_ref()
            .map(|errors| errors as &(dyn Error + 'static))
    }
}
