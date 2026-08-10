use rux_domain::{SemanticVersion, TargetOs};
use rux_manifest::{
    BuildMode, DefineValue, DependencySource, MANIFEST_MAX_BYTES, ManifestErrorCode, ManifestKind,
    ManifestPath, Optimization, PackageType, ValidationProfile, parse_manifest,
    parse_manifest_with_profile,
};

const HEADER: &str = r#"[Manifest]
Version = 1
MinRux = "0.4.0"
"#;

#[test]
fn complete_package_parses_into_typed_values() {
    let source = r#"[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Namespace = "Rux"
Name = "Example_App"
Version = "1.2.3-alpha.1+linux"
Type = "Source"
Description = "Example package"
Authors = ["Rux Contributors <info@rux-lang.dev>"]
Keywords = ["Registry", "Package-Manager"]
License = "MIT OR Apache-2.0"
LicenseFile = "LICENSE.md"
Repository = "https://github.com/rux-lang/example"
Homepage = "https://example.dev/docs"
ReadmeFile = "README.md"

[Dependencies]
Io = { Namespace = "Rux", Version = "^1.0" }
JsonAlias = { Namespace = "Acme", Package = "FastJson", Version = "2" }
LocalUtil = { Package = "Util", Path = "../Util" }

[Build]
Output = "Artifacts"

[Build.Defines]
Channel = "Nightly"
Retries = 3

[Build.Debug]
DebugAssertions = false

[Build.Debug.Defines]
Tracing = true

[Build.Release]
Optimization = "Size"
DebugInfo = true
Output = "Dist"

[Build.Release.Defines]
Channel = "Stable"
"#;

    let manifest = parse_manifest(source).expect("complete manifest should parse");
    assert_eq!(manifest.header().version(), 1);
    assert_eq!(manifest.header().min_rux(), Some(&version("0.4.0")));
    assert!(manifest.is_supported_by(&version("0.4.0")));
    assert!(!manifest.is_supported_by(&version("0.4.0-alpha")));

    let ManifestKind::Package(package) = manifest.kind() else {
        panic!("expected package manifest");
    };
    assert_eq!(
        package.namespace().map(ToString::to_string).as_deref(),
        Some("Rux")
    );
    assert_eq!(package.name().as_str(), "Example_App");
    assert_eq!(package.name().normalized(), "example-app");
    assert_eq!(package.version().as_str(), "1.2.3-alpha.1+linux");
    assert_eq!(package.package_type(), PackageType::Source);
    assert_eq!(package.description(), Some("Example package"));
    assert_eq!(package.authors().len(), 1);
    assert_eq!(package.keywords()[0].normalized(), "registry");
    assert_eq!(package.license(), Some("MIT OR Apache-2.0"));
    let path = ManifestPath::as_str;
    assert_eq!(package.license_file().map(path), Some("LICENSE.md"));
    assert_eq!(package.readme_file().map(path), Some("README.md"));
    let repository = package.repository().expect("repository");
    assert_eq!(repository.as_str(), "https://github.com/rux-lang/example");

    let dependencies = package.dependencies().iter().collect::<Vec<_>>();
    assert_eq!(dependencies.len(), 3);
    assert_eq!(dependencies[0].0.normalized(), "io");
    assert_eq!(dependencies[1].0.normalized(), "jsonalias");
    assert_eq!(dependencies[2].0.normalized(), "localutil");
    assert_eq!(dependencies[0].1.package().as_str(), "Io");
    assert!(dependencies[0].1.target_os().is_empty());
    assert!(
        matches!(dependencies[0].1.source(), DependencySource::Registry { namespace, version } if namespace.as_str() == "Rux" && version.as_str() == "^1.0")
    );
    assert!(
        matches!(dependencies[2].1.source(), DependencySource::Path(path) if path.as_str() == "../Util")
    );

    let debug = package.build().resolve(BuildMode::Debug);
    assert_eq!(debug.optimization(), Optimization::None);
    assert!(!debug.debug_assertions());
    assert!(debug.debug_info());
    assert_eq!(debug.output().as_str(), "Artifacts/Debug");
    assert_eq!(
        debug.defines().get("Channel"),
        Some(&DefineValue::String("Nightly".to_owned()))
    );
    assert_eq!(
        debug.defines().get("Tracing"),
        Some(&DefineValue::Boolean(true))
    );
    assert_eq!(
        debug.defines().get("Retries"),
        Some(&DefineValue::Integer(3))
    );

    let release = package.build().resolve(BuildMode::Release);
    assert_eq!(release.optimization(), Optimization::Size);
    assert!(release.debug_info());
    assert!(!release.debug_assertions());
    assert_eq!(release.output().as_str(), "Dist");
    assert_eq!(
        release.defines().get("Channel"),
        Some(&DefineValue::String("Stable".to_owned()))
    );
}

#[test]
fn workspace_requires_safe_nonempty_package_paths() {
    let source = format!("{HEADER}\n[Workspace]\nPackages = [\"Packages/Core\", \"Tests/Core\"]\n");
    let manifest = parse_manifest(&source).expect("workspace should parse");
    let ManifestKind::Workspace(workspace) = manifest.kind() else {
        panic!("expected workspace manifest");
    };
    assert_eq!(workspace.packages()[0].as_str(), "Packages/Core");
    assert_eq!(workspace.packages()[1].as_str(), "Tests/Core");

    let invalid = format!("{HEADER}\n[Workspace]\nPackages = [\"../Outside\"]\n");
    assert_eq!(codes(&invalid), vec![ManifestErrorCode::InvalidPath]);
}

#[test]
fn local_profile_preserves_namespace_workspace_and_path_dependency_behavior() {
    let package = format!(
        r#"{HEADER}
[Package]
Name = "Example"
Version = "1.0.0"
Type = "Source"

[Dependencies]
LocalUtil = {{ Path = "../Util" }}
"#
    );
    assert!(parse_manifest(&package).is_ok());
    assert!(parse_manifest_with_profile(&package, ValidationProfile::Local).is_ok());

    let workspace = format!("{HEADER}\n[Workspace]\nPackages = [\"Packages/Core\"]\n");
    assert!(parse_manifest_with_profile(&workspace, ValidationProfile::Local).is_ok());
}

#[test]
fn publication_profile_accepts_namespaced_registry_packages() {
    let source = format!(
        r#"{HEADER}
[Package]
Namespace = "Rux"
Name = "Example"
Version = "1.0.0"
Type = "Source"

[Dependencies]
Io = {{ Namespace = "Rux", Version = "^1.0" }}
"#
    );

    assert!(parse_manifest_with_profile(&source, ValidationProfile::Publication).is_ok());
}

#[test]
fn publication_profile_locates_and_sorts_all_package_blockers() {
    let source = format!(
        r#"{HEADER}
[Package]
Name = "Example"
Version = "1.0.0"
Type = "Source"

[Dependencies]
Zed = {{ Path = "../Zed" }}
Alpha = {{ Path = "../Alpha" }}
"#
    );
    let errors = parse_manifest_with_profile(&source, ValidationProfile::Publication)
        .expect_err("publication blockers should fail");
    let errors = errors.as_slice();

    assert_eq!(errors.len(), 3);
    assert_eq!(errors[0].code(), ManifestErrorCode::MissingField);
    assert_eq!(errors[0].path(), &["Package", "Namespace"]);
    assert_eq!(
        errors[0].message(),
        "publication requires Package.Namespace"
    );
    assert_eq!(errors[0].span().start().line(), 5);
    assert_eq!(errors[1].code(), ManifestErrorCode::NotPublishable);
    assert_eq!(errors[1].path(), &["Dependencies", "Zed", "Path"]);
    assert_eq!(errors[1].message(), "path dependencies cannot be published");
    assert_eq!(errors[1].span().start().line(), 11);
    assert_eq!(errors[2].code(), ManifestErrorCode::NotPublishable);
    assert_eq!(errors[2].path(), &["Dependencies", "Alpha", "Path"]);
    assert_eq!(errors[2].span().start().line(), 12);
}

#[test]
fn publication_profile_rejects_workspace_at_its_source() {
    let source = format!("{HEADER}\n[Workspace]\nPackages = [\"Packages/Core\"]\n");
    let errors = parse_manifest_with_profile(&source, ValidationProfile::Publication)
        .expect_err("workspace cannot be published");
    let error = &errors.as_slice()[0];

    assert_eq!(errors.as_slice().len(), 1);
    assert_eq!(error.code(), ManifestErrorCode::NotPublishable);
    assert_eq!(error.code().as_str(), "not_publishable");
    assert_eq!(error.path(), &["Workspace"]);
    assert_eq!(error.message(), "workspace manifests cannot be published");
    assert_eq!(error.span().start().line(), 5);
}

#[test]
fn publication_profile_does_not_cascade_from_base_schema_errors() {
    let source = format!(
        r#"{HEADER}
[Package]
Name = "bad name"
Version = "1.0.0"
Type = "Source"

[Dependencies]
LocalUtil = {{ Path = "../Util" }}
"#
    );
    let errors = parse_manifest_with_profile(&source, ValidationProfile::Publication)
        .expect_err("invalid package identity should fail base validation");

    assert_eq!(
        errors
            .as_slice()
            .iter()
            .map(rux_manifest::ManifestError::code)
            .collect::<Vec<_>>(),
        vec![ManifestErrorCode::InvalidIdentity]
    );
}

#[test]
fn min_rux_cannot_predate_manifest_v1() {
    let package = "\n[Package]\nName = \"Example\"\nVersion = \"1.0.0\"\nType = \"Source\"\n";
    for minimum in ["0.3.99", "0.4.0-alpha.1"] {
        let source = format!("[Manifest]\nVersion = 1\nMinRux = \"{minimum}\"\n{package}");
        assert_eq!(codes(&source), vec![ManifestErrorCode::MinimumRuxTooOld]);
    }

    // Build metadata does not affect the floor comparison.
    let source = format!("[Manifest]\nVersion = 1\nMinRux = \"0.4.0+local\"\n{package}");
    assert!(parse_manifest(&source).is_ok());
}

#[test]
fn min_rux_is_optional_locally_and_required_for_publication() {
    let package = "\n[Package]\nNamespace = \"Rux\"\nName = \"Example\"\nVersion = \"1.0.0\"\nType = \"Source\"\n";
    let source = format!("[Manifest]\nVersion = 1\n{package}");

    // A locally-built package need not carry a field only publication uses,
    // and no declared minimum means every compiler version is supported.
    let manifest = parse_manifest(&source).expect("MinRux is optional for local validation");
    assert!(manifest.header().min_rux().is_none());
    assert!(manifest.is_supported_by(&version("0.1.0")));

    let errors = parse_manifest_with_profile(&source, ValidationProfile::Publication)
        .expect_err("publication requires MinRux");
    let error = &errors.as_slice()[0];
    assert_eq!(errors.as_slice().len(), 1);
    assert_eq!(error.code(), ManifestErrorCode::MissingField);
    assert_eq!(error.path(), &["Manifest", "MinRux"]);
    assert_eq!(error.message(), "publication requires Manifest.MinRux");
    assert_eq!(error.span().start().line(), 1);
}

#[test]
fn only_debug_and_release_build_tables_are_allowed() {
    let package = format!(
        "{HEADER}\n[Package]\nName = \"Example\"\nVersion = \"1.0.0\"\nType = \"Program\"\n"
    );

    let custom = format!("{package}\n[Build.Production]\nOptimization = \"Speed\"\n");
    let errors = parse_manifest(&custom).expect_err("custom profile must fail");
    assert_eq!(errors.as_slice()[0].code(), ManifestErrorCode::UnknownField);
    assert_eq!(errors.as_slice()[0].path(), &["Build", "Production"]);

    let mode = format!("{package}\n[Build.Debug]\nMode = \"Debug\"\n");
    assert_eq!(codes(&mode), vec![ManifestErrorCode::UnknownField]);

    let lowercase = format!("{package}\n[Build.debug]\nDebugInfo = true\n");
    assert_eq!(codes(&lowercase), vec![ManifestErrorCode::UnknownField]);
}

#[test]
fn only_program_library_and_source_package_types_are_allowed() {
    for (source, expected) in [
        ("Program", PackageType::Program),
        ("Library", PackageType::Library),
        ("Source", PackageType::Source),
    ] {
        let manifest = parse_manifest(&format!(
            "{HEADER}\n[Package]\nName = \"Example\"\nVersion = \"1.0.0\"\nType = \"{source}\"\n"
        ))
        .expect("documented package type should parse");
        let ManifestKind::Package(package) = manifest.kind() else {
            panic!("expected a package manifest");
        };
        assert_eq!(package.package_type(), expected);
    }

    for legacy in ["Binary", "StaticLibrary", "SharedLibrary"] {
        let source = format!(
            "{HEADER}\n[Package]\nName = \"Example\"\nVersion = \"1.0.0\"\nType = \"{legacy}\"\n"
        );
        assert_eq!(codes(&source), vec![ManifestErrorCode::InvalidPackageType]);
    }
}

#[test]
fn dependency_sources_are_explicit_and_aliases_cannot_collide() {
    let source = format!(
        r#"{HEADER}
[Package]
Name = "Example"
Version = "1.0.0"
Type = "Source"

[Dependencies]
Foo_Bar = {{ Namespace = "Rux", Version = "1" }}
foo-bar = {{ Path = "../Foo" }}
Missing = {{ Namespace = "Rux" }}
Mixed = {{ Namespace = "Rux", Version = "1", Path = "../Mixed" }}
"#
    );
    assert_eq!(
        codes(&source),
        vec![
            ManifestErrorCode::NormalizedCollision,
            ManifestErrorCode::MissingField,
            ManifestErrorCode::InvalidDependencySource,
        ]
    );
}

#[test]
fn dependency_target_operating_systems_are_exact_nonempty_unique_lists() {
    let accepted = format!(
        r#"{HEADER}
[Package]
Name = "Example"
Version = "1.0.0"
Type = "Source"

[Dependencies]
Platform = {{ Path = "../Platform", TargetOS = ["Windows", "Linux", "MacOS", "FreeBSD", "OpenBSD", "NetBSD", "DragonFlyBSD", "Illumos"] }}
"#
    );
    let manifest = parse_manifest(&accepted).expect("supported TargetOS values should parse");
    let ManifestKind::Package(package) = manifest.kind() else {
        panic!("expected package manifest");
    };
    assert_eq!(
        package
            .dependencies()
            .values()
            .next()
            .expect("platform dependency")
            .target_os(),
        &[
            TargetOs::Windows,
            TargetOs::Linux,
            TargetOs::MacOs,
            TargetOs::FreeBsd,
            TargetOs::OpenBsd,
            TargetOs::NetBsd,
            TargetOs::DragonFlyBsd,
            TargetOs::Illumos,
        ]
    );

    for (value, expected) in [
        ("[]", ManifestErrorCode::EmptyValue),
        (
            "[\"Windows\", \"Windows\"]",
            ManifestErrorCode::NormalizedCollision,
        ),
        ("[\"macOS\"]", ManifestErrorCode::InvalidTargetOs),
        ("\"Windows\"", ManifestErrorCode::WrongType),
        ("[1]", ManifestErrorCode::WrongType),
    ] {
        let source = format!(
            "{HEADER}\n[Package]\nName = \"Example\"\nVersion = \"1.0.0\"\nType = \"Source\"\n\n[Dependencies]\nPlatform = {{ Path = \"../Platform\", TargetOS = {value} }}\n"
        );
        assert_eq!(codes(&source), vec![expected]);
    }
}

#[test]
fn errors_are_source_located_and_sorted_by_source_order() {
    let source = r#"[Manifest]
Version = 2
MinRux = "0.3.0"

[Package]
Name = "bad name"
Version = "01.0.0"
Type = "source"
Unknown = true
"#;
    let errors = parse_manifest(source).expect_err("manifest should fail");
    let errors = errors.as_slice();
    assert_eq!(
        errors[0].code(),
        ManifestErrorCode::UnsupportedManifestVersion
    );
    assert_eq!(errors[0].path(), &["Manifest", "Version"]);
    assert_eq!(errors[0].span().start().line(), 2);
    assert_eq!(errors[1].code(), ManifestErrorCode::MinimumRuxTooOld);
    assert_eq!(errors[1].span().start().line(), 3);
    assert_eq!(errors[2].code(), ManifestErrorCode::InvalidIdentity);
    assert_eq!(errors[3].code(), ManifestErrorCode::InvalidSemanticVersion);
    assert_eq!(errors[4].code(), ManifestErrorCode::InvalidPackageType);
    assert_eq!(errors[5].code(), ManifestErrorCode::UnknownField);
    assert!(
        errors
            .windows(2)
            .all(|pair| { pair[0].span().start().byte() <= pair[1].span().start().byte() })
    );
}

#[test]
fn metadata_and_paths_are_strict() {
    let source = format!(
        r#"{HEADER}
[Package]
Name = "Example"
Version = "1.0.0"
Type = "Source"
License = "not-a-license"
LicenseFile = "../LICENSE"
Repository = "ssh://git@example.com/repo"
Homepage = "https://user:secret@example.com"
ReadmeFile = 'docs\README.md'
"#
    );
    assert_eq!(
        codes(&source),
        vec![
            ManifestErrorCode::InvalidSpdxExpression,
            ManifestErrorCode::InvalidPath,
            ManifestErrorCode::InvalidUrl,
            ManifestErrorCode::InvalidUrl,
            ManifestErrorCode::InvalidPath,
        ]
    );
}

#[test]
fn syntax_and_source_size_fail_with_one_diagnostic() {
    let syntax = "[Manifest\nVersion = 1";
    let errors = parse_manifest(syntax).expect_err("syntax should fail");
    assert_eq!(errors.as_slice().len(), 1);
    assert_eq!(errors.as_slice()[0].code(), ManifestErrorCode::TomlSyntax);

    let oversized = "x".repeat(MANIFEST_MAX_BYTES + 1);
    let errors = parse_manifest(&oversized).expect_err("size should fail");
    assert_eq!(errors.as_slice().len(), 1);
    assert_eq!(
        errors.as_slice()[0].code(),
        ManifestErrorCode::ManifestTooLarge
    );
}

#[test]
fn legacy_unversioned_manifest_is_rejected() {
    let source = r#"[Package]
Name = "Legacy"
Version = "0.1.0"
Type = "Source"
"#;
    assert_eq!(codes(source), vec![ManifestErrorCode::MissingField]);
}

fn codes(source: &str) -> Vec<ManifestErrorCode> {
    parse_manifest(source)
        .expect_err("fixture should be invalid")
        .as_slice()
        .iter()
        .map(rux_manifest::ManifestError::code)
        .collect()
}

fn version(value: &str) -> SemanticVersion {
    SemanticVersion::new(value).expect("version fixture should be valid")
}
