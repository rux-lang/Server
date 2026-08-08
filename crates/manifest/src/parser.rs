use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use rux_domain::{IdentitySegment, SemanticVersion, VersionRange};
use toml_edit::{Document, InlineTable, Item, Table, Value};
use url::Url;

use crate::error::{ManifestError, ManifestErrorCode, ManifestErrors, SourcePosition, SourceSpan};
use crate::model::{
    BuildConfiguration, BuildOverrides, DefineValue, Dependency, DependencySource, Manifest,
    ManifestHeader, ManifestKind, ManifestPath, Optimization, PackageManifest, PackageType, WebUrl,
    WorkspaceManifest,
};
use crate::{
    MANIFEST_MAX_AUTHOR_BYTES, MANIFEST_MAX_AUTHORS, MANIFEST_MAX_BYTES,
    MANIFEST_MAX_DEFINE_STRING_BYTES, MANIFEST_MAX_DEFINES, MANIFEST_MAX_DEPENDENCIES,
    MANIFEST_MAX_DESCRIPTION_BYTES, MANIFEST_MAX_KEYWORD_OR_DEFINE_NAME_BYTES,
    MANIFEST_MAX_KEYWORDS, MANIFEST_MAX_LICENSE_OR_RANGE_BYTES,
    MANIFEST_MAX_SEMANTIC_VERSION_BYTES, MANIFEST_MAX_URL_OR_PATH_BYTES,
    MANIFEST_MAX_WORKSPACE_PACKAGES, MANIFEST_MIN_RUX_VERSION, MANIFEST_VERSION,
};

/// Selects the rules applied after the common manifest schema is validated.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationProfile {
    /// Accepts packages and workspaces used during local development.
    Local,
    /// Accepts only manifests that can be submitted to the registry.
    Publication,
}

/// Parses a complete in-memory `Rux.toml` document using local validation.
///
/// # Errors
///
/// Returns stable, source-located diagnostics when the TOML or manifest schema
/// is invalid.
pub fn parse_manifest(source: &str) -> Result<Manifest, ManifestErrors> {
    parse_manifest_with_profile(source, ValidationProfile::Local)
}

/// Parses a complete in-memory `Rux.toml` document using `profile`.
///
/// # Errors
///
/// Returns stable, source-located diagnostics when the TOML, manifest schema,
/// or selected validation profile is invalid.
pub fn parse_manifest_with_profile(
    source: &str,
    profile: ValidationProfile,
) -> Result<Manifest, ManifestErrors> {
    if source.len() > MANIFEST_MAX_BYTES {
        let locator = Locator::new(source);
        return Err(ManifestErrors::new(vec![ManifestError::new(
            ManifestErrorCode::ManifestTooLarge,
            Vec::new(),
            locator.span(0..0),
            format!(
                "manifest is {} bytes; maximum is {MANIFEST_MAX_BYTES}",
                source.len()
            ),
        )]));
    }

    let document = source.parse::<Document<String>>().map_err(|error| {
        let locator = Locator::new(source);
        let range = error.span().unwrap_or(0..0);
        ManifestErrors::new(vec![ManifestError::new(
            ManifestErrorCode::TomlSyntax,
            Vec::new(),
            locator.span(range),
            error.message().to_owned(),
        )])
    })?;

    Parser::new(source).parse(document.as_table(), profile)
}

struct Parser<'source> {
    locator: Locator<'source>,
    errors: Vec<ManifestError>,
}

impl<'source> Parser<'source> {
    fn new(source: &'source str) -> Self {
        Self {
            locator: Locator::new(source),
            errors: Vec::new(),
        }
    }

    fn parse(
        mut self,
        root: &Table,
        profile: ValidationProfile,
    ) -> Result<Manifest, ManifestErrors> {
        self.reject_unknown_table_keys(
            root,
            &["Manifest", "Package", "Workspace", "Dependencies", "Build"],
            &[],
        );

        let header = self.parse_header(root);
        let package_item = root.get("Package");
        let workspace_item = root.get("Workspace");

        let kind = match (package_item, workspace_item) {
            (Some(package), Some(workspace)) => {
                self.push(
                    ManifestErrorCode::ConflictingSections,
                    path(&["Workspace"]),
                    workspace.span(),
                    "Package and Workspace sections are mutually exclusive",
                );
                let _ = self.parse_package(root, package);
                None
            }
            (Some(package), None) => self
                .parse_package(root, package)
                .map(Box::new)
                .map(ManifestKind::Package),
            (None, Some(workspace)) => {
                if let Some(item) = root.get("Dependencies") {
                    self.push(
                        ManifestErrorCode::ConflictingSections,
                        path(&["Dependencies"]),
                        item.span(),
                        "workspace manifests cannot declare Dependencies",
                    );
                }
                if let Some(item) = root.get("Build") {
                    self.push(
                        ManifestErrorCode::ConflictingSections,
                        path(&["Build"]),
                        item.span(),
                        "workspace manifests cannot declare Build configuration",
                    );
                }
                self.parse_workspace(workspace).map(ManifestKind::Workspace)
            }
            (None, None) => {
                self.push(
                    ManifestErrorCode::MissingField,
                    vec!["Package|Workspace".to_owned()],
                    None,
                    "manifest must contain either Package or Workspace",
                );
                None
            }
        };

        let manifest = header
            .zip(kind)
            .map(|(header, kind)| Manifest::new(header, kind));
        if self.errors.is_empty()
            && let Some(manifest) = manifest.as_ref()
        {
            self.validate_profile(root, manifest, profile);
        }
        self.finish(manifest)
    }

    fn validate_profile(&mut self, root: &Table, manifest: &Manifest, profile: ValidationProfile) {
        if profile == ValidationProfile::Local {
            return;
        }

        if manifest.header().min_rux().is_none() {
            self.push(
                ManifestErrorCode::MissingField,
                path(&["Manifest", "MinRux"]),
                root.get("Manifest").and_then(Item::span),
                "publication requires Manifest.MinRux",
            );
        }

        match manifest.kind() {
            ManifestKind::Workspace(_) => self.push(
                ManifestErrorCode::NotPublishable,
                path(&["Workspace"]),
                root.get("Workspace").and_then(Item::span),
                "workspace manifests cannot be published",
            ),
            ManifestKind::Package(package) => {
                if package.namespace().is_none() {
                    self.push(
                        ManifestErrorCode::MissingField,
                        path(&["Package", "Namespace"]),
                        root.get("Package").and_then(Item::span),
                        "publication requires Package.Namespace",
                    );
                }

                let dependency_table = root.get("Dependencies").and_then(Item::as_table);
                for (alias, dependency) in package.dependencies() {
                    if !matches!(dependency.source(), DependencySource::Path(_)) {
                        continue;
                    }
                    let span = dependency_table
                        .and_then(|table| table.get(alias.as_str()))
                        .and_then(Item::as_value)
                        .and_then(Value::as_inline_table)
                        .and_then(|table| table.get("Path"))
                        .and_then(Value::span);
                    self.push_owned(
                        ManifestErrorCode::NotPublishable,
                        vec![
                            "Dependencies".to_owned(),
                            alias.as_str().to_owned(),
                            "Path".to_owned(),
                        ],
                        span,
                        "path dependencies cannot be published",
                    );
                }
            }
        }
    }

    fn finish(mut self, manifest: Option<Manifest>) -> Result<Manifest, ManifestErrors> {
        if self.errors.is_empty() {
            return Ok(manifest.expect("a manifest is constructed when validation succeeds"));
        }
        self.errors.sort_by(|left, right| {
            left.span()
                .start()
                .byte()
                .cmp(&right.span().start().byte())
                .then_with(|| left.span().end().byte().cmp(&right.span().end().byte()))
                .then_with(|| left.code().as_str().cmp(right.code().as_str()))
                .then_with(|| left.path().cmp(right.path()))
        });
        Err(ManifestErrors::new(self.errors))
    }

    fn parse_header(&mut self, root: &Table) -> Option<ManifestHeader> {
        let Some(item) = root.get("Manifest") else {
            self.push(
                ManifestErrorCode::MissingField,
                path(&["Manifest"]),
                None,
                "missing required Manifest section",
            );
            return None;
        };
        let table = self.expect_table(item, &["Manifest"])?;
        self.reject_unknown_table_keys(table, &["Version", "MinRux"], &["Manifest"]);

        let version = self.required_integer(table, "Version", &["Manifest"]);
        let version = version.and_then(|value| match u16::try_from(value) {
            Ok(value) if value == MANIFEST_VERSION => Some(value),
            _ => {
                self.push(
                    ManifestErrorCode::UnsupportedManifestVersion,
                    path(&["Manifest", "Version"]),
                    table.get("Version").and_then(Item::span),
                    format!("only manifest version {MANIFEST_VERSION} is supported"),
                );
                None
            }
        });

        // MinRux is optional for a locally-built package and required for
        // publication, which `validate_profile` enforces. A declared value is
        // held to the floor either way.
        let min_rux = table
            .get("MinRux")
            .and_then(|item| {
                self.item_string(item, &["Manifest", "MinRux"])
                    .and_then(|(value, span)| {
                        self.parse_semantic_version(
                            value,
                            span,
                            &["Manifest", "MinRux"],
                            MANIFEST_MAX_SEMANTIC_VERSION_BYTES,
                        )
                    })
            })
            .and_then(|value| {
                let floor = SemanticVersion::new(MANIFEST_MIN_RUX_VERSION)
                    .expect("manifest minimum Rux constant is valid");
                if value.cmp_precedence(&floor) == Ordering::Less {
                    self.push(
                        ManifestErrorCode::MinimumRuxTooOld,
                        path(&["Manifest", "MinRux"]),
                        table.get("MinRux").and_then(Item::span),
                        format!("MinRux must be at least {MANIFEST_MIN_RUX_VERSION}"),
                    );
                    None
                } else {
                    Some(value)
                }
            });

        version.map(|version| ManifestHeader::new(version, min_rux))
    }

    fn parse_package(&mut self, root: &Table, item: &Item) -> Option<PackageManifest> {
        let table = self.expect_table(item, &["Package"])?;
        self.reject_unknown_table_keys(
            table,
            &[
                "Namespace",
                "Name",
                "Version",
                "Type",
                "Description",
                "Authors",
                "Keywords",
                "License",
                "LicenseUrl",
                "Repository",
                "Homepage",
                "Readme",
            ],
            &["Package"],
        );

        let namespace = self.optional_identity(table, "Namespace", &["Package"]);
        let name = self.required_identity(table, "Name", &["Package"]);
        let version = self
            .required_string(table, "Version", &["Package"])
            .and_then(|(value, span)| {
                self.parse_semantic_version(
                    value,
                    span,
                    &["Package", "Version"],
                    MANIFEST_MAX_SEMANTIC_VERSION_BYTES,
                )
            });
        let package_type = self.parse_package_type(table);
        let description = self.optional_bounded_string(
            table,
            "Description",
            &["Package"],
            MANIFEST_MAX_DESCRIPTION_BYTES,
        );
        let authors = self.parse_authors(table);
        let keywords = self.parse_keywords(table);
        let license = self.parse_license(table);
        let license_url = self.optional_url(table, "LicenseUrl", &["Package"]);
        let repository = self.optional_url(table, "Repository", &["Package"]);
        let homepage = self.optional_url(table, "Homepage", &["Package"]);
        let readme = self.optional_path(table, "Readme", &["Package"], false);
        let dependencies = root.get("Dependencies").map_or_else(
            || Some(BTreeMap::new()),
            |item| self.parse_dependencies(item),
        );
        let build = root.get("Build").map_or_else(
            || Some(BuildConfiguration::default()),
            |item| self.parse_build(item),
        );

        Some(PackageManifest {
            namespace,
            name: name?,
            version: version?,
            package_type: package_type?,
            description,
            authors: authors?,
            keywords: keywords?,
            license,
            license_url,
            repository,
            homepage,
            readme,
            dependencies: dependencies?,
            build: build?,
        })
    }

    fn parse_workspace(&mut self, item: &Item) -> Option<WorkspaceManifest> {
        let table = self.expect_table(item, &["Workspace"])?;
        self.reject_unknown_table_keys(table, &["Packages"], &["Workspace"]);
        let Some(packages_item) = table.get("Packages") else {
            self.push(
                ManifestErrorCode::MissingField,
                path(&["Workspace", "Packages"]),
                item.span(),
                "missing required Packages array",
            );
            return None;
        };
        let Some(array) = packages_item.as_value().and_then(Value::as_array) else {
            self.wrong_type(
                &["Workspace", "Packages"],
                packages_item.span(),
                "an array of strings",
            );
            return None;
        };
        if array.is_empty() {
            self.push(
                ManifestErrorCode::EmptyValue,
                path(&["Workspace", "Packages"]),
                packages_item.span(),
                "workspace Packages cannot be empty",
            );
        }
        self.check_count(
            array.len(),
            MANIFEST_MAX_WORKSPACE_PACKAGES,
            &["Workspace", "Packages"],
            packages_item.span(),
        );

        let mut packages = Vec::new();
        let mut seen = BTreeSet::new();
        let mut valid = true;
        for (index, value) in array.iter().enumerate() {
            let entry_path = vec![
                "Workspace".to_owned(),
                "Packages".to_owned(),
                index.to_string(),
            ];
            let Some(value) = value.as_str() else {
                self.wrong_type_owned(entry_path, value.span(), "a string");
                valid = false;
                continue;
            };
            if let Some(parsed) = self.validate_path(
                value,
                value_span(value, array, index),
                &["Workspace", "Packages"],
                false,
            ) {
                if seen.insert(parsed.as_str().to_owned()) {
                    packages.push(parsed);
                } else {
                    self.push_owned(
                        ManifestErrorCode::NormalizedCollision,
                        vec![
                            "Workspace".to_owned(),
                            "Packages".to_owned(),
                            index.to_string(),
                        ],
                        array.get(index).and_then(Value::span),
                        "workspace package path is duplicated",
                    );
                    valid = false;
                }
            } else {
                valid = false;
            }
        }
        valid.then_some(WorkspaceManifest { packages })
    }

    fn parse_package_type(&mut self, table: &Table) -> Option<PackageType> {
        let (value, span) = self.required_string(table, "Type", &["Package"])?;
        match value {
            "Program" => Some(PackageType::Program),
            "Library" => Some(PackageType::Library),
            "Source" => Some(PackageType::Source),
            _ => {
                self.push(
                    ManifestErrorCode::InvalidPackageType,
                    path(&["Package", "Type"]),
                    span,
                    "Type must be Program, Library, or Source",
                );
                None
            }
        }
    }

    fn parse_authors(&mut self, table: &Table) -> Option<Vec<String>> {
        let Some(item) = table.get("Authors") else {
            return Some(Vec::new());
        };
        let Some(array) = item.as_value().and_then(Value::as_array) else {
            self.wrong_type(&["Package", "Authors"], item.span(), "an array of strings");
            return None;
        };
        self.check_count(
            array.len(),
            MANIFEST_MAX_AUTHORS,
            &["Package", "Authors"],
            item.span(),
        );
        let mut result = Vec::new();
        let mut valid = true;
        for (index, value) in array.iter().enumerate() {
            let entry_path = vec![
                "Package".to_owned(),
                "Authors".to_owned(),
                index.to_string(),
            ];
            let Some(value) = value.as_str() else {
                self.wrong_type_owned(entry_path, value.span(), "a string");
                valid = false;
                continue;
            };
            if self.validate_nonempty_bounded(
                value,
                MANIFEST_MAX_AUTHOR_BYTES,
                entry_path,
                array.get(index).and_then(Value::span),
            ) {
                result.push(value.to_owned());
            } else {
                valid = false;
            }
        }
        valid.then_some(result)
    }

    fn parse_keywords(&mut self, table: &Table) -> Option<Vec<IdentitySegment>> {
        let Some(item) = table.get("Keywords") else {
            return Some(Vec::new());
        };
        let Some(array) = item.as_value().and_then(Value::as_array) else {
            self.wrong_type(&["Package", "Keywords"], item.span(), "an array of strings");
            return None;
        };
        self.check_count(
            array.len(),
            MANIFEST_MAX_KEYWORDS,
            &["Package", "Keywords"],
            item.span(),
        );
        let mut result = Vec::new();
        let mut seen = BTreeSet::new();
        let mut valid = true;
        for (index, value) in array.iter().enumerate() {
            let entry_path = vec![
                "Package".to_owned(),
                "Keywords".to_owned(),
                index.to_string(),
            ];
            let Some(value) = value.as_str() else {
                self.wrong_type_owned(entry_path, value.span(), "a string");
                valid = false;
                continue;
            };
            if value.len() > MANIFEST_MAX_KEYWORD_OR_DEFINE_NAME_BYTES {
                self.push_owned(
                    ManifestErrorCode::ValueTooLong,
                    entry_path,
                    array.get(index).and_then(Value::span),
                    format!("keyword exceeds {MANIFEST_MAX_KEYWORD_OR_DEFINE_NAME_BYTES} bytes"),
                );
                valid = false;
                continue;
            }
            match IdentitySegment::new(value) {
                Ok(keyword) if seen.insert(keyword.clone()) => result.push(keyword),
                Ok(_) => {
                    self.push_owned(
                        ManifestErrorCode::NormalizedCollision,
                        entry_path,
                        array.get(index).and_then(Value::span),
                        "keyword collides after normalization",
                    );
                    valid = false;
                }
                Err(error) => {
                    self.push_owned(
                        ManifestErrorCode::InvalidIdentity,
                        entry_path,
                        array.get(index).and_then(Value::span),
                        format!("invalid keyword: {error}"),
                    );
                    valid = false;
                }
            }
        }
        valid.then_some(result)
    }

    fn parse_license(&mut self, table: &Table) -> Option<String> {
        let item = table.get("License")?;
        let (value, span) = self.item_string(item, &["Package", "License"])?;
        if !self.validate_nonempty_bounded(
            value,
            MANIFEST_MAX_LICENSE_OR_RANGE_BYTES,
            path(&["Package", "License"]),
            span.clone(),
        ) {
            return None;
        }
        if let Err(error) = spdx::Expression::parse(value) {
            self.push(
                ManifestErrorCode::InvalidSpdxExpression,
                path(&["Package", "License"]),
                span,
                format!("invalid SPDX expression: {error}"),
            );
            None
        } else {
            Some(value.to_owned())
        }
    }

    fn parse_dependencies(&mut self, item: &Item) -> Option<BTreeMap<IdentitySegment, Dependency>> {
        let table = self.expect_table(item, &["Dependencies"])?;
        self.check_count(
            table.len(),
            MANIFEST_MAX_DEPENDENCIES,
            &["Dependencies"],
            item.span(),
        );
        let mut result = BTreeMap::new();
        let mut aliases = BTreeSet::new();
        let mut valid = true;
        for (alias_source, dependency_item) in table {
            let key_span = table
                .get_key_value(alias_source)
                .and_then(|(key, _)| key.span())
                .or_else(|| dependency_item.span());
            let alias_path = vec!["Dependencies".to_owned(), alias_source.to_owned()];
            let alias = match IdentitySegment::new(alias_source) {
                Ok(alias) if aliases.insert(alias.clone()) => alias,
                Ok(_) => {
                    self.push_owned(
                        ManifestErrorCode::NormalizedCollision,
                        alias_path,
                        key_span,
                        "dependency alias collides after normalization",
                    );
                    valid = false;
                    continue;
                }
                Err(error) => {
                    self.push_owned(
                        ManifestErrorCode::InvalidIdentity,
                        alias_path,
                        key_span,
                        format!("invalid dependency alias: {error}"),
                    );
                    valid = false;
                    continue;
                }
            };
            let Some(inline) = dependency_item.as_value().and_then(Value::as_inline_table) else {
                self.wrong_type_owned(
                    vec!["Dependencies".to_owned(), alias_source.to_owned()],
                    dependency_item.span(),
                    "an inline table",
                );
                valid = false;
                continue;
            };
            if let Some(dependency) = self.parse_dependency(alias_source, &alias, inline) {
                result.insert(alias, dependency);
            } else {
                valid = false;
            }
        }
        valid.then_some(result)
    }

    fn parse_dependency(
        &mut self,
        alias_source: &str,
        alias: &IdentitySegment,
        table: &InlineTable,
    ) -> Option<Dependency> {
        self.reject_unknown_inline_keys(
            table,
            &["Namespace", "Package", "Version", "Path"],
            &["Dependencies", alias_source],
        );
        let package = self
            .optional_inline_identity(table, "Package", alias_source)
            .unwrap_or_else(|| alias.clone());
        let path_value = table.get("Path");
        let namespace_value = table.get("Namespace");
        let version_value = table.get("Version");

        let source = if let Some(path_value) = path_value {
            if namespace_value.is_some() || version_value.is_some() {
                self.push_owned(
                    ManifestErrorCode::InvalidDependencySource,
                    vec!["Dependencies".to_owned(), alias_source.to_owned()],
                    table.span(),
                    "path dependencies cannot declare Namespace or Version",
                );
                None
            } else {
                self.inline_string(path_value, &["Dependencies", alias_source, "Path"])
                    .and_then(|(value, span)| {
                        self.validate_path(
                            value,
                            span,
                            &["Dependencies", alias_source, "Path"],
                            true,
                        )
                    })
                    .map(DependencySource::Path)
            }
        } else {
            let namespace = namespace_value.and_then(|value| {
                self.inline_string(value, &["Dependencies", alias_source, "Namespace"])
                    .and_then(|(value, span)| {
                        self.parse_identity(
                            value,
                            span,
                            &["Dependencies", alias_source, "Namespace"],
                        )
                    })
            });
            if namespace_value.is_none() {
                self.missing_inline(table, &["Dependencies", alias_source, "Namespace"]);
            }
            let version = version_value.and_then(|value| {
                self.inline_string(value, &["Dependencies", alias_source, "Version"])
                    .and_then(|(value, span)| {
                        if !self.validate_nonempty_bounded(
                            value,
                            MANIFEST_MAX_LICENSE_OR_RANGE_BYTES,
                            dynamic_path(&["Dependencies", alias_source, "Version"]),
                            span.clone(),
                        ) {
                            return None;
                        }
                        match VersionRange::new(value) {
                            Ok(value) => Some(value),
                            Err(error) => {
                                self.push_owned(
                                    ManifestErrorCode::InvalidVersionRange,
                                    dynamic_path(&["Dependencies", alias_source, "Version"]),
                                    span,
                                    format!("invalid dependency version range: {error}"),
                                );
                                None
                            }
                        }
                    })
            });
            if version_value.is_none() {
                self.missing_inline(table, &["Dependencies", alias_source, "Version"]);
            }
            namespace
                .zip(version)
                .map(|(namespace, version)| DependencySource::Registry { namespace, version })
        };

        source.map(|source| Dependency { package, source })
    }

    fn parse_build(&mut self, item: &Item) -> Option<BuildConfiguration> {
        let table = self.expect_table(item, &["Build"])?;
        self.reject_unknown_table_keys(
            table,
            &["Output", "Defines", "Debug", "Release"],
            &["Build"],
        );
        let output = self
            .optional_path(table, "Output", &["Build"], true)
            .unwrap_or_else(|| ManifestPath::new("Bin".to_owned()));
        let defines = table.get("Defines").map_or_else(
            || Some(BTreeMap::new()),
            |item| self.parse_defines(item, &["Build", "Defines"]),
        );
        let debug = table.get("Debug").map_or_else(
            || Some(BuildOverrides::default()),
            |item| self.parse_build_overrides(item, "Debug"),
        );
        let release = table.get("Release").map_or_else(
            || Some(BuildOverrides::default()),
            |item| self.parse_build_overrides(item, "Release"),
        );

        Some(BuildConfiguration {
            output,
            defines: defines?,
            debug: debug?,
            release: release?,
        })
    }

    fn parse_build_overrides(&mut self, item: &Item, mode: &str) -> Option<BuildOverrides> {
        let table = self.expect_table(item, &["Build", mode])?;
        self.reject_unknown_table_keys(
            table,
            &[
                "Optimization",
                "DebugInfo",
                "DebugAssertions",
                "Output",
                "Defines",
            ],
            &["Build", mode],
        );
        let optimization = table.get("Optimization").and_then(|item| {
            self.item_string(item, &["Build", mode, "Optimization"])
                .and_then(|(value, span)| match value {
                    "None" => Some(Optimization::None),
                    "Size" => Some(Optimization::Size),
                    "Speed" => Some(Optimization::Speed),
                    _ => {
                        self.push_owned(
                            ManifestErrorCode::InvalidOptimization,
                            dynamic_path(&["Build", mode, "Optimization"]),
                            span,
                            "Optimization must be None, Size, or Speed",
                        );
                        None
                    }
                })
        });
        let debug_info = self.optional_boolean(table, "DebugInfo", &["Build", mode]);
        let debug_assertions = self.optional_boolean(table, "DebugAssertions", &["Build", mode]);
        let output = self.optional_path(table, "Output", &["Build", mode], true);
        let defines = table.get("Defines").map_or_else(
            || Some(BTreeMap::new()),
            |item| self.parse_defines(item, &["Build", mode, "Defines"]),
        );
        Some(BuildOverrides {
            optimization,
            debug_info,
            debug_assertions,
            output,
            defines: defines?,
        })
    }

    fn parse_defines(
        &mut self,
        item: &Item,
        field_path: &[&str],
    ) -> Option<BTreeMap<String, DefineValue>> {
        let table = self.expect_table(item, field_path)?;
        self.check_count(table.len(), MANIFEST_MAX_DEFINES, field_path, item.span());
        let mut result = BTreeMap::new();
        let mut valid = true;
        for (name, value) in table {
            let mut value_path = dynamic_path(field_path);
            value_path.push(name.to_owned());
            let key_span = table
                .get_key_value(name)
                .and_then(|(key, _)| key.span())
                .or_else(|| value.span());
            if !valid_define_name(name) {
                self.push_owned(
                    ManifestErrorCode::InvalidDefineName,
                    value_path,
                    key_span,
                    format!(
                        "define name must be a 1-{MANIFEST_MAX_KEYWORD_OR_DEFINE_NAME_BYTES} byte ASCII identifier"
                    ),
                );
                valid = false;
                continue;
            }
            let Some(value) = value.as_value() else {
                self.wrong_type_owned(value_path, value.span(), "a string, Boolean, or integer");
                valid = false;
                continue;
            };
            let parsed = if let Some(value) = value.as_str() {
                if value.len() > MANIFEST_MAX_DEFINE_STRING_BYTES {
                    self.push_owned(
                        ManifestErrorCode::ValueTooLong,
                        value_path.clone(),
                        value_span_direct(value, table.get(name)),
                        format!("define string exceeds {MANIFEST_MAX_DEFINE_STRING_BYTES} bytes"),
                    );
                    None
                } else {
                    Some(DefineValue::String(value.to_owned()))
                }
            } else if let Some(value) = value.as_bool() {
                Some(DefineValue::Boolean(value))
            } else {
                value.as_integer().map(DefineValue::Integer)
            };
            if let Some(parsed) = parsed {
                result.insert(name.to_owned(), parsed);
            } else if !matches!(value, Value::String(_)) {
                self.wrong_type_owned(value_path, value.span(), "a string, Boolean, or integer");
                valid = false;
            } else {
                valid = false;
            }
        }
        valid.then_some(result)
    }

    fn optional_identity(
        &mut self,
        table: &Table,
        key: &str,
        parent: &[&str],
    ) -> Option<IdentitySegment> {
        table.get(key).and_then(|item| {
            let mut field_path = dynamic_path(parent);
            field_path.push(key.to_owned());
            self.item_string_owned(item, field_path.clone())
                .and_then(|(value, span)| self.parse_identity_owned(value, span, field_path))
        })
    }

    fn required_identity(
        &mut self,
        table: &Table,
        key: &str,
        parent: &[&str],
    ) -> Option<IdentitySegment> {
        self.required_string(table, key, parent)
            .and_then(|(value, span)| {
                let mut field_path = dynamic_path(parent);
                field_path.push(key.to_owned());
                self.parse_identity_owned(value, span, field_path)
            })
    }

    fn optional_inline_identity(
        &mut self,
        table: &InlineTable,
        key: &str,
        alias: &str,
    ) -> Option<IdentitySegment> {
        table.get(key).and_then(|value| {
            self.inline_string(value, &["Dependencies", alias, key])
                .and_then(|(value, span)| {
                    self.parse_identity(value, span, &["Dependencies", alias, key])
                })
        })
    }

    fn parse_identity(
        &mut self,
        value: &str,
        span: Option<Range<usize>>,
        field_path: &[&str],
    ) -> Option<IdentitySegment> {
        self.parse_identity_owned(value, span, dynamic_path(field_path))
    }

    fn parse_identity_owned(
        &mut self,
        value: &str,
        span: Option<Range<usize>>,
        field_path: Vec<String>,
    ) -> Option<IdentitySegment> {
        match IdentitySegment::new(value) {
            Ok(value) => Some(value),
            Err(error) => {
                self.push_owned(
                    ManifestErrorCode::InvalidIdentity,
                    field_path,
                    span,
                    format!("invalid identity segment: {error}"),
                );
                None
            }
        }
    }

    fn parse_semantic_version(
        &mut self,
        value: &str,
        span: Option<Range<usize>>,
        field_path: &[&str],
        maximum: usize,
    ) -> Option<SemanticVersion> {
        if !self.validate_nonempty_bounded(value, maximum, dynamic_path(field_path), span.clone()) {
            return None;
        }
        match SemanticVersion::new(value) {
            Ok(value) => Some(value),
            Err(error) => {
                self.push_owned(
                    ManifestErrorCode::InvalidSemanticVersion,
                    dynamic_path(field_path),
                    span,
                    format!("invalid semantic version: {error}"),
                );
                None
            }
        }
    }

    fn optional_url(&mut self, table: &Table, key: &str, parent: &[&str]) -> Option<WebUrl> {
        let item = table.get(key)?;
        let mut field_path = dynamic_path(parent);
        field_path.push(key.to_owned());
        let (value, span) = self.item_string_owned(item, field_path.clone())?;
        if !self.validate_nonempty_bounded(
            value,
            MANIFEST_MAX_URL_OR_PATH_BYTES,
            field_path.clone(),
            span.clone(),
        ) {
            return None;
        }
        match Url::parse(value) {
            Ok(url)
                if matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none() =>
            {
                Some(WebUrl::new(value.to_owned()))
            }
            Ok(_) | Err(_) => {
                self.push_owned(
                    ManifestErrorCode::InvalidUrl,
                    field_path,
                    span,
                    "URL must be absolute HTTP(S), have a host, and contain no credentials",
                );
                None
            }
        }
    }

    fn optional_path(
        &mut self,
        table: &Table,
        key: &str,
        parent: &[&str],
        allow_parent: bool,
    ) -> Option<ManifestPath> {
        let item = table.get(key)?;
        let mut field_path = dynamic_path(parent);
        field_path.push(key.to_owned());
        let (value, span) = self.item_string_owned(item, field_path.clone())?;
        self.validate_path_owned(value, span, field_path, allow_parent)
    }

    fn validate_path(
        &mut self,
        value: &str,
        span: Option<Range<usize>>,
        field_path: &[&str],
        allow_parent: bool,
    ) -> Option<ManifestPath> {
        self.validate_path_owned(value, span, dynamic_path(field_path), allow_parent)
    }

    fn validate_path_owned(
        &mut self,
        value: &str,
        span: Option<Range<usize>>,
        field_path: Vec<String>,
        allow_parent: bool,
    ) -> Option<ManifestPath> {
        let components = value.split('/').collect::<Vec<_>>();
        let parent_after_regular = allow_parent
            && components
                .iter()
                .skip_while(|component| **component == "..")
                .any(|component| *component == "..");
        let invalid = value.is_empty()
            || value.len() > MANIFEST_MAX_URL_OR_PATH_BYTES
            || value.contains(['\\', '\0'])
            || value.starts_with('/')
            || value.get(1..2) == Some(":")
            || components
                .iter()
                .any(|component| component.is_empty() || *component == ".")
            || (!allow_parent && components.contains(&".."))
            || parent_after_regular;
        if invalid {
            self.push_owned(
                ManifestErrorCode::InvalidPath,
                field_path,
                span,
                if allow_parent {
                    "path must be a non-empty portable relative path using '/'"
                } else {
                    "path must stay below its root and use portable '/' components"
                },
            );
            None
        } else {
            Some(ManifestPath::new(value.to_owned()))
        }
    }

    fn optional_bounded_string(
        &mut self,
        table: &Table,
        key: &str,
        parent: &[&str],
        maximum: usize,
    ) -> Option<String> {
        let item = table.get(key)?;
        let mut field_path = dynamic_path(parent);
        field_path.push(key.to_owned());
        let (value, span) = self.item_string_owned(item, field_path.clone())?;
        self.validate_nonempty_bounded(value, maximum, field_path, span)
            .then(|| value.to_owned())
    }

    fn optional_boolean(&mut self, table: &Table, key: &str, parent: &[&str]) -> Option<bool> {
        let item = table.get(key)?;
        let Some(value) = item.as_value().and_then(Value::as_bool) else {
            let mut field_path = dynamic_path(parent);
            field_path.push(key.to_owned());
            self.wrong_type_owned(field_path, item.span(), "a Boolean");
            return None;
        };
        Some(value)
    }

    fn required_string<'item>(
        &mut self,
        table: &'item Table,
        key: &str,
        parent: &[&str],
    ) -> Option<(&'item str, Option<Range<usize>>)> {
        let Some(item) = table.get(key) else {
            let mut field_path = dynamic_path(parent);
            field_path.push(key.to_owned());
            self.push_owned(
                ManifestErrorCode::MissingField,
                field_path,
                table.span(),
                format!("missing required {key} field"),
            );
            return None;
        };
        let mut field_path = dynamic_path(parent);
        field_path.push(key.to_owned());
        self.item_string_owned(item, field_path)
    }

    fn required_integer(&mut self, table: &Table, key: &str, parent: &[&str]) -> Option<i64> {
        let Some(item) = table.get(key) else {
            let mut field_path = dynamic_path(parent);
            field_path.push(key.to_owned());
            self.push_owned(
                ManifestErrorCode::MissingField,
                field_path,
                table.span(),
                format!("missing required {key} field"),
            );
            return None;
        };
        let Some(value) = item.as_value().and_then(Value::as_integer) else {
            let mut field_path = dynamic_path(parent);
            field_path.push(key.to_owned());
            self.wrong_type_owned(field_path, item.span(), "an integer");
            return None;
        };
        Some(value)
    }

    fn item_string<'item>(
        &mut self,
        item: &'item Item,
        field_path: &[&str],
    ) -> Option<(&'item str, Option<Range<usize>>)> {
        self.item_string_owned(item, dynamic_path(field_path))
    }

    fn item_string_owned<'item>(
        &mut self,
        item: &'item Item,
        field_path: Vec<String>,
    ) -> Option<(&'item str, Option<Range<usize>>)> {
        let Some(value) = item.as_value().and_then(Value::as_str) else {
            self.wrong_type_owned(field_path, item.span(), "a string");
            return None;
        };
        Some((value, item.span()))
    }

    fn inline_string<'item>(
        &mut self,
        value: &'item Value,
        field_path: &[&str],
    ) -> Option<(&'item str, Option<Range<usize>>)> {
        let Some(string) = value.as_str() else {
            self.wrong_type_owned(dynamic_path(field_path), value.span(), "a string");
            return None;
        };
        Some((string, value.span()))
    }

    fn validate_nonempty_bounded(
        &mut self,
        value: &str,
        maximum: usize,
        field_path: Vec<String>,
        span: Option<Range<usize>>,
    ) -> bool {
        if value.trim().is_empty() {
            self.push_owned(
                ManifestErrorCode::EmptyValue,
                field_path,
                span,
                "value cannot be empty or whitespace-only",
            );
            false
        } else if value.len() > maximum {
            self.push_owned(
                ManifestErrorCode::ValueTooLong,
                field_path,
                span,
                format!("value is {} bytes; maximum is {maximum}", value.len()),
            );
            false
        } else {
            true
        }
    }

    fn check_count(
        &mut self,
        actual: usize,
        maximum: usize,
        field_path: &[&str],
        span: Option<Range<usize>>,
    ) {
        if actual > maximum {
            self.push_owned(
                ManifestErrorCode::TooManyItems,
                dynamic_path(field_path),
                span,
                format!("contains {actual} items; maximum is {maximum}"),
            );
        }
    }

    fn expect_table<'item>(
        &mut self,
        item: &'item Item,
        field_path: &[&str],
    ) -> Option<&'item Table> {
        let Some(table) = item.as_table() else {
            self.wrong_type(field_path, item.span(), "a table");
            return None;
        };
        Some(table)
    }

    fn reject_unknown_table_keys(&mut self, table: &Table, allowed: &[&str], parent: &[&str]) {
        for (name, item) in table {
            if !allowed.contains(&name) {
                let mut field_path = dynamic_path(parent);
                field_path.push(name.to_owned());
                let span = table
                    .get_key_value(name)
                    .and_then(|(key, _)| key.span())
                    .or_else(|| item.span());
                self.push_owned(
                    ManifestErrorCode::UnknownField,
                    field_path,
                    span,
                    format!("unknown field or section {name}"),
                );
            }
        }
    }

    fn reject_unknown_inline_keys(
        &mut self,
        table: &InlineTable,
        allowed: &[&str],
        parent: &[&str],
    ) {
        for (name, value) in table {
            if !allowed.contains(&name) {
                let mut field_path = dynamic_path(parent);
                field_path.push(name.to_owned());
                let span = table
                    .get_key_value(name)
                    .and_then(|(key, _)| key.span())
                    .or_else(|| value.span());
                self.push_owned(
                    ManifestErrorCode::UnknownField,
                    field_path,
                    span,
                    format!("unknown dependency field {name}"),
                );
            }
        }
    }

    fn missing_inline(&mut self, table: &InlineTable, field_path: &[&str]) {
        let name = field_path.last().copied().unwrap_or("field");
        self.push_owned(
            ManifestErrorCode::MissingField,
            dynamic_path(field_path),
            table.span(),
            format!("missing required {name} field"),
        );
    }

    fn wrong_type(&mut self, field_path: &[&str], span: Option<Range<usize>>, expected: &str) {
        self.wrong_type_owned(dynamic_path(field_path), span, expected);
    }

    fn wrong_type_owned(
        &mut self,
        field_path: Vec<String>,
        span: Option<Range<usize>>,
        expected: &str,
    ) {
        self.push_owned(
            ManifestErrorCode::WrongType,
            field_path,
            span,
            format!("expected {expected}"),
        );
    }

    fn push(
        &mut self,
        code: ManifestErrorCode,
        field_path: Vec<String>,
        span: Option<Range<usize>>,
        message: impl Into<String>,
    ) {
        self.push_owned(code, field_path, span, message);
    }

    fn push_owned(
        &mut self,
        code: ManifestErrorCode,
        field_path: Vec<String>,
        span: Option<Range<usize>>,
        message: impl Into<String>,
    ) {
        self.errors.push(ManifestError::new(
            code,
            field_path,
            self.locator.span(span.unwrap_or(0..0)),
            message.into(),
        ));
    }
}

struct Locator<'source> {
    source: &'source str,
    line_starts: Vec<usize>,
}

impl<'source> Locator<'source> {
    fn new(source: &'source str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            source,
            line_starts,
        }
    }

    fn span(&self, range: Range<usize>) -> SourceSpan {
        SourceSpan::new(self.position(range.start), self.position(range.end))
    }

    fn position(&self, requested: usize) -> SourcePosition {
        let mut byte = requested.min(self.source.len());
        while byte > 0 && !self.source.is_char_boundary(byte) {
            byte -= 1;
        }
        let line_index = self.line_starts.partition_point(|start| *start <= byte) - 1;
        let line_start = self.line_starts[line_index];
        let column = self.source[line_start..byte].chars().count() + 1;
        SourcePosition::new(byte, line_index + 1, column)
    }
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn dynamic_path(parts: &[&str]) -> Vec<String> {
    path(parts)
}

fn valid_define_name(value: &str) -> bool {
    if value.is_empty() || value.len() > MANIFEST_MAX_KEYWORD_OR_DEFINE_NAME_BYTES {
        return false;
    }
    let mut bytes = value.bytes();
    let first = bytes.next().expect("non-empty string has a first byte");
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn value_span(_value: &str, array: &toml_edit::Array, index: usize) -> Option<Range<usize>> {
    array.get(index).and_then(Value::span)
}

fn value_span_direct(_value: &str, item: Option<&Item>) -> Option<Range<usize>> {
    item.and_then(Item::span)
}
