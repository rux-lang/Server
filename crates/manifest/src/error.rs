use std::error::Error;
use std::fmt;

/// Stable machine-readable categories for manifest failures.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ManifestErrorCode {
    ManifestTooLarge,
    TomlSyntax,
    MissingField,
    UnknownField,
    WrongType,
    UnsupportedManifestVersion,
    ConflictingSections,
    InvalidIdentity,
    InvalidSemanticVersion,
    InvalidVersionRange,
    InvalidPackageType,
    InvalidSpdxExpression,
    InvalidUrl,
    InvalidPath,
    EmptyValue,
    ValueTooLong,
    TooManyItems,
    NormalizedCollision,
    InvalidDependencySource,
    InvalidOptimization,
    InvalidDefineName,
    MinimumRuxTooOld,
    NotPublishable,
}

impl ManifestErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestTooLarge => "manifest_too_large",
            Self::TomlSyntax => "toml_syntax",
            Self::MissingField => "missing_field",
            Self::UnknownField => "unknown_field",
            Self::WrongType => "wrong_type",
            Self::UnsupportedManifestVersion => "unsupported_manifest_version",
            Self::ConflictingSections => "conflicting_sections",
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidSemanticVersion => "invalid_semantic_version",
            Self::InvalidVersionRange => "invalid_version_range",
            Self::InvalidPackageType => "invalid_package_type",
            Self::InvalidSpdxExpression => "invalid_spdx_expression",
            Self::InvalidUrl => "invalid_url",
            Self::InvalidPath => "invalid_path",
            Self::EmptyValue => "empty_value",
            Self::ValueTooLong => "value_too_long",
            Self::TooManyItems => "too_many_items",
            Self::NormalizedCollision => "normalized_collision",
            Self::InvalidDependencySource => "invalid_dependency_source",
            Self::InvalidOptimization => "invalid_optimization",
            Self::InvalidDefineName => "invalid_define_name",
            Self::MinimumRuxTooOld => "minimum_rux_too_old",
            Self::NotPublishable => "not_publishable",
        }
    }
}

/// A position in the original UTF-8 manifest source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourcePosition {
    byte: usize,
    line: usize,
    column: usize,
}

impl SourcePosition {
    pub(crate) const fn new(byte: usize, line: usize, column: usize) -> Self {
        Self { byte, line, column }
    }

    #[must_use]
    pub const fn byte(self) -> usize {
        self.byte
    }

    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }
}

/// An end-exclusive source range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpan {
    start: SourcePosition,
    end: SourcePosition,
}

impl SourceSpan {
    pub(crate) const fn new(start: SourcePosition, end: SourcePosition) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn start(self) -> SourcePosition {
        self.start
    }

    #[must_use]
    pub const fn end(self) -> SourcePosition {
        self.end
    }
}

/// One located manifest diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestError {
    code: ManifestErrorCode,
    path: Vec<String>,
    span: SourceSpan,
    message: String,
}

impl ManifestError {
    pub(crate) fn new(
        code: ManifestErrorCode,
        path: Vec<String>,
        span: SourceSpan,
        message: String,
    ) -> Self {
        Self {
            code,
            path,
            span,
            message,
        }
    }

    #[must_use]
    pub const fn code(&self) -> ManifestErrorCode {
        self.code
    }

    #[must_use]
    pub fn path(&self) -> &[String] {
        &self.path
    }

    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}:{}: {}",
            self.code.as_str(),
            self.span.start.line,
            self.span.start.column,
            self.message
        )
    }
}

/// A non-empty, deterministically ordered set of manifest errors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestErrors(Vec<ManifestError>);

impl ManifestErrors {
    pub(crate) fn new(errors: Vec<ManifestError>) -> Self {
        debug_assert!(!errors.is_empty());
        Self(errors)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[ManifestError] {
        &self.0
    }
}

impl AsRef<[ManifestError]> for ManifestErrors {
    fn as_ref(&self) -> &[ManifestError] {
        self.as_slice()
    }
}

impl IntoIterator for ManifestErrors {
    type Item = ManifestError;
    type IntoIter = std::vec::IntoIter<ManifestError>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl fmt::Display for ManifestErrors {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "manifest contains {} error(s)", self.0.len())
    }
}

impl Error for ManifestErrors {}
