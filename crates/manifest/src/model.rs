use std::collections::BTreeMap;
use std::fmt;

use rux_domain::{IdentitySegment, SemanticVersion, VersionRange};

/// A parsed and validated versioned Rux manifest.
#[derive(Clone, Debug)]
pub struct Manifest {
    header: ManifestHeader,
    kind: ManifestKind,
}

impl Manifest {
    pub(crate) fn new(header: ManifestHeader, kind: ManifestKind) -> Self {
        Self { header, kind }
    }

    /// Returns the version and compiler-compatibility header.
    #[must_use]
    pub fn header(&self) -> &ManifestHeader {
        &self.header
    }

    /// Returns the package or workspace contents.
    #[must_use]
    pub fn kind(&self) -> &ManifestKind {
        &self.kind
    }

    /// Reports whether `compiler_version` satisfies this manifest's minimum.
    ///
    /// A manifest that declares no minimum is supported by every compiler.
    #[must_use]
    pub fn is_supported_by(&self, compiler_version: &SemanticVersion) -> bool {
        self.header.min_rux().is_none_or(|minimum| {
            compiler_version.cmp_precedence(minimum) != std::cmp::Ordering::Less
        })
    }
}

/// Version information from `[Manifest]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestHeader {
    version: u16,
    min_rux: Option<SemanticVersion>,
}

impl ManifestHeader {
    pub(crate) fn new(version: u16, min_rux: Option<SemanticVersion>) -> Self {
        Self { version, min_rux }
    }

    /// Returns the integer manifest schema version.
    #[must_use]
    pub fn version(&self) -> u16 {
        self.version
    }

    /// Returns the minimum compatible Rux compiler version.
    ///
    /// The field is optional for a locally-built package and required for
    /// publication, so a manifest accepted by
    /// [`ValidationProfile::Publication`](crate::ValidationProfile) always
    /// carries one.
    #[must_use]
    pub fn min_rux(&self) -> Option<&SemanticVersion> {
        self.min_rux.as_ref()
    }
}

/// The mutually exclusive top-level manifest contents.
#[derive(Clone, Debug)]
pub enum ManifestKind {
    /// A buildable package.
    Package(Box<PackageManifest>),
    /// A collection of local package paths.
    Workspace(WorkspaceManifest),
}

/// A package manifest and its registry metadata.
#[derive(Clone, Debug)]
pub struct PackageManifest {
    pub(crate) namespace: Option<IdentitySegment>,
    pub(crate) name: IdentitySegment,
    pub(crate) version: SemanticVersion,
    pub(crate) package_type: PackageType,
    pub(crate) description: Option<String>,
    pub(crate) authors: Vec<String>,
    pub(crate) keywords: Vec<IdentitySegment>,
    pub(crate) license: Option<License>,
    pub(crate) repository: Option<WebUrl>,
    pub(crate) homepage: Option<WebUrl>,
    pub(crate) readme: Option<ManifestPath>,
    pub(crate) dependencies: BTreeMap<IdentitySegment, Dependency>,
    pub(crate) build: BuildConfiguration,
}

impl PackageManifest {
    #[must_use]
    pub fn namespace(&self) -> Option<&IdentitySegment> {
        self.namespace.as_ref()
    }

    #[must_use]
    pub fn name(&self) -> &IdentitySegment {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &SemanticVersion {
        &self.version
    }

    #[must_use]
    pub fn package_type(&self) -> PackageType {
        self.package_type
    }

    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    #[must_use]
    pub fn authors(&self) -> &[String] {
        &self.authors
    }

    #[must_use]
    pub fn keywords(&self) -> &[IdentitySegment] {
        &self.keywords
    }

    #[must_use]
    pub fn license(&self) -> Option<&License> {
        self.license.as_ref()
    }

    #[must_use]
    pub fn repository(&self) -> Option<&WebUrl> {
        self.repository.as_ref()
    }

    #[must_use]
    pub fn homepage(&self) -> Option<&WebUrl> {
        self.homepage.as_ref()
    }

    #[must_use]
    pub fn readme(&self) -> Option<&ManifestPath> {
        self.readme.as_ref()
    }

    #[must_use]
    pub fn dependencies(&self) -> &BTreeMap<IdentitySegment, Dependency> {
        &self.dependencies
    }

    #[must_use]
    pub fn build(&self) -> &BuildConfiguration {
        &self.build
    }
}

/// The compiler output produced by a package.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageType {
    /// A runnable executable with a `Main` entry point.
    Program,
    /// A dynamic library linked by dependents and loaded at run time.
    Library,
    /// Rux sources compiled directly into dependent packages.
    Source,
}

/// Package licensing metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum License {
    Expression(String),
    File(ManifestPath),
}

/// A validated catalog URL whose submitted spelling is preserved.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WebUrl(String);

impl WebUrl {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WebUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A validated portable relative path whose submitted spelling is preserved.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ManifestPath(String);

impl ManifestPath {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ManifestPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A dependency target. The containing map key is the local import alias.
#[derive(Clone, Debug)]
pub struct Dependency {
    pub(crate) package: IdentitySegment,
    pub(crate) source: DependencySource,
}

impl Dependency {
    #[must_use]
    pub fn package(&self) -> &IdentitySegment {
        &self.package
    }

    #[must_use]
    pub fn source(&self) -> &DependencySource {
        &self.source
    }
}

/// The mutually exclusive source of a dependency.
#[derive(Clone, Debug)]
pub enum DependencySource {
    Registry {
        namespace: IdentitySegment,
        version: VersionRange,
    },
    Path(ManifestPath),
}

impl DependencySource {
    #[must_use]
    pub fn namespace(&self) -> Option<&IdentitySegment> {
        match self {
            Self::Registry { namespace, .. } => Some(namespace),
            Self::Path(_) => None,
        }
    }

    #[must_use]
    pub fn version(&self) -> Option<&VersionRange> {
        match self {
            Self::Registry { version, .. } => Some(version),
            Self::Path(_) => None,
        }
    }

    #[must_use]
    pub fn path(&self) -> Option<&ManifestPath> {
        match self {
            Self::Registry { .. } => None,
            Self::Path(path) => Some(path),
        }
    }
}

/// A manifest containing local package members.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceManifest {
    pub(crate) packages: Vec<ManifestPath>,
}

impl WorkspaceManifest {
    #[must_use]
    pub fn packages(&self) -> &[ManifestPath] {
        &self.packages
    }
}

/// The only supported build modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildMode {
    Debug,
    Release,
}

impl BuildMode {
    fn directory_name(self) -> &'static str {
        match self {
            Self::Debug => "Debug",
            Self::Release => "Release",
        }
    }
}

/// Compiler optimization behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Optimization {
    None,
    Size,
    Speed,
}

/// A typed compile-time definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DefineValue {
    String(String),
    Boolean(bool),
    Integer(i64),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BuildOverrides {
    pub(crate) optimization: Option<Optimization>,
    pub(crate) debug_info: Option<bool>,
    pub(crate) debug_assertions: Option<bool>,
    pub(crate) output: Option<ManifestPath>,
    pub(crate) defines: BTreeMap<String, DefineValue>,
}

/// Shared build configuration plus Debug and Release overrides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildConfiguration {
    pub(crate) output: ManifestPath,
    pub(crate) defines: BTreeMap<String, DefineValue>,
    pub(crate) debug: BuildOverrides,
    pub(crate) release: BuildOverrides,
}

impl Default for BuildConfiguration {
    fn default() -> Self {
        Self {
            output: ManifestPath::new("Bin".to_owned()),
            defines: BTreeMap::new(),
            debug: BuildOverrides::default(),
            release: BuildOverrides::default(),
        }
    }
}

impl BuildConfiguration {
    #[must_use]
    pub fn output(&self) -> &ManifestPath {
        &self.output
    }

    #[must_use]
    pub fn defines(&self) -> &BTreeMap<String, DefineValue> {
        &self.defines
    }

    /// Resolves built-in defaults, shared configuration, and mode overrides.
    #[must_use]
    pub fn resolve(&self, mode: BuildMode) -> EffectiveBuildConfiguration {
        let overrides = match mode {
            BuildMode::Debug => &self.debug,
            BuildMode::Release => &self.release,
        };
        let (default_optimization, default_debug_info, default_debug_assertions) = match mode {
            BuildMode::Debug => (Optimization::None, true, true),
            BuildMode::Release => (Optimization::Speed, false, false),
        };
        let output = overrides.output.clone().unwrap_or_else(|| {
            ManifestPath::new(format!(
                "{}/{}",
                self.output.as_str(),
                mode.directory_name()
            ))
        });
        let mut defines = self.defines.clone();
        defines.extend(overrides.defines.clone());

        EffectiveBuildConfiguration {
            mode,
            optimization: overrides.optimization.unwrap_or(default_optimization),
            debug_info: overrides.debug_info.unwrap_or(default_debug_info),
            debug_assertions: overrides
                .debug_assertions
                .unwrap_or(default_debug_assertions),
            output,
            defines,
        }
    }
}

/// Fully resolved settings for Debug or Release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBuildConfiguration {
    mode: BuildMode,
    optimization: Optimization,
    debug_info: bool,
    debug_assertions: bool,
    output: ManifestPath,
    defines: BTreeMap<String, DefineValue>,
}

impl EffectiveBuildConfiguration {
    #[must_use]
    pub fn mode(&self) -> BuildMode {
        self.mode
    }

    #[must_use]
    pub fn optimization(&self) -> Optimization {
        self.optimization
    }

    #[must_use]
    pub fn debug_info(&self) -> bool {
        self.debug_info
    }

    #[must_use]
    pub fn debug_assertions(&self) -> bool {
        self.debug_assertions
    }

    #[must_use]
    pub fn output(&self) -> &ManifestPath {
        &self.output
    }

    #[must_use]
    pub fn defines(&self) -> &BTreeMap<String, DefineValue> {
        &self.defines
    }
}
