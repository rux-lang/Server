use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use rux_domain::IdentitySegment;

/// The allowlisted `(root, namespace)` pairs a source imports.
type ResolvedDependencies<'a> = BTreeMap<&'a IdentitySegment, &'a IdentitySegment>;

/// Package name given to every generated playground manifest.
pub const PLAYGROUND_PACKAGE_NAME: &str = "Playground";
/// Package version given to every generated playground manifest.
pub const PLAYGROUND_PACKAGE_VERSION: &str = "0.0.0";
/// Manifest schema version emitted by [`manifest_for`].
pub const PLAYGROUND_MANIFEST_VERSION: u16 = rux_domain::MANIFEST_VERSION;
/// Minimum compiler version declared by generated manifests.
pub const PLAYGROUND_MIN_RUX_VERSION: &str = "0.4.0";

/// The set of packages a playground run may depend on.
///
/// The sandbox has no network, so only packages already seeded into the image
/// can resolve. Entries are keyed by the import root as it appears in
/// `import <Root>::`; lookup is case-insensitive and folds `_` to `-` because
/// [`IdentitySegment`] compares on its normalized form.
#[derive(Clone, Debug, Default)]
pub struct PackageAllowlist {
    entries: BTreeMap<IdentitySegment, IdentitySegment>,
}

impl PackageAllowlist {
    /// Builds an allowlist from `(root, namespace)` pairs.
    ///
    /// A later duplicate root replaces an earlier one.
    #[must_use]
    pub fn new(entries: impl IntoIterator<Item = (IdentitySegment, IdentitySegment)>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// Returns the namespace registered for `root`, if any.
    #[must_use]
    pub fn namespace_for(&self, root: &IdentitySegment) -> Option<&IdentitySegment> {
        self.entries.get(root)
    }

    /// Reports whether the allowlist has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Renders the `Rux.toml` for one playground run.
///
/// Import roots are read from `source` and intersected with `allowlist`. A root
/// that is not allowlisted is left out so the compiler produces its own
/// diagnostic about the unresolved import, which is more useful to the author
/// than a manifest error. Dependency spellings come from the allowlist rather
/// than from the source, so no submitted text reaches the rendered file.
///
/// The output is deterministic: dependencies are ordered by their normalized
/// identity, and identical input always renders byte-identical output.
#[must_use]
pub fn manifest_for(source: &str, allowlist: &PackageAllowlist) -> String {
    let mut manifest = format!(
        "[Manifest]\nVersion = {PLAYGROUND_MANIFEST_VERSION}\nMinRux = \"{PLAYGROUND_MIN_RUX_VERSION}\"\n\n\
         [Package]\nName = \"{PLAYGROUND_PACKAGE_NAME}\"\nVersion = \"{PLAYGROUND_PACKAGE_VERSION}\"\nType = \"Executable\"\n"
    );

    let dependencies = resolve_dependencies(source, allowlist);
    if !dependencies.is_empty() {
        manifest.push_str("\n[Dependencies]\n");
        for (root, namespace) in dependencies {
            // Writing into a `String` is infallible.
            let _ = writeln!(
                manifest,
                "{root} = {{ Namespace = \"{namespace}\", Version = \"*\" }}"
            );
        }
    }

    manifest
}

/// Returns the allowlisted `(root, namespace)` pairs `source` imports.
fn resolve_dependencies<'a>(
    source: &str,
    allowlist: &'a PackageAllowlist,
) -> ResolvedDependencies<'a> {
    import_roots(source)
        .iter()
        .filter_map(|root| allowlist.entries.get_key_value(root))
        .collect()
}

/// Extracts the first path segment of every `import <Root>::` in `source`.
///
/// This is a deliberately shallow scan: it does not parse Rux, so an `import`
/// inside a comment or a string literal is treated as real. The consequence is
/// bounded — at worst an allowlisted dependency the program never uses is
/// declared, and the sandbox has no network for that to matter.
fn import_roots(source: &str) -> BTreeSet<IdentitySegment> {
    let mut roots = BTreeSet::new();

    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("import") else {
            continue;
        };
        // `import` must be a whole word, not the prefix of a longer identifier.
        if !rest.starts_with(|character: char| character.is_whitespace()) {
            continue;
        }

        let rest = rest.trim_start();
        let root: String = rest
            .chars()
            .take_while(|character| {
                character.is_ascii_alphanumeric() || *character == '-' || *character == '_'
            })
            .collect();

        if root.is_empty() || !rest[root.len()..].starts_with("::") {
            continue;
        }

        if let Ok(segment) = IdentitySegment::new(root) {
            roots.insert(segment);
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use rux_domain::IdentitySegment;
    use rux_manifest::{MANIFEST_MIN_RUX_VERSION, MANIFEST_VERSION, ManifestKind, parse_manifest};

    use super::{
        PLAYGROUND_MANIFEST_VERSION, PLAYGROUND_MIN_RUX_VERSION, PackageAllowlist, import_roots,
        manifest_for,
    };

    fn segment(value: &str) -> IdentitySegment {
        IdentitySegment::new(value).expect("test segment should be valid")
    }

    fn allowlist() -> PackageAllowlist {
        PackageAllowlist::new([
            (segment("Std"), segment("Rux")),
            (segment("Json"), segment("Rux")),
            (segment("Http"), segment("Contrib")),
        ])
    }

    #[test]
    fn the_generated_constants_track_the_manifest_crate() {
        assert_eq!(PLAYGROUND_MANIFEST_VERSION, MANIFEST_VERSION);
        assert_eq!(PLAYGROUND_MIN_RUX_VERSION, MANIFEST_MIN_RUX_VERSION);
    }

    #[test]
    fn a_manifest_without_imports_declares_no_dependencies_and_parses() {
        let rendered = manifest_for("Fn Main() {}\n", &allowlist());

        assert!(!rendered.contains("[Dependencies]"));
        assert_eq!(
            rendered,
            "[Manifest]\nVersion = 1\nMinRux = \"0.4.0\"\n\n\
             [Package]\nName = \"Playground\"\nVersion = \"0.0.0\"\nType = \"Executable\"\n"
        );
        assert!(parse_manifest(&rendered).is_ok());
    }

    #[test]
    fn allowlisted_imports_become_sorted_registry_dependencies() {
        let source = "import Std::Io\nimport Http::Client\nimport Json::Encode\n\nFn Main() {}\n";
        let rendered = manifest_for(source, &allowlist());

        assert!(rendered.ends_with(
            "\n[Dependencies]\n\
             Http = { Namespace = \"Contrib\", Version = \"*\" }\n\
             Json = { Namespace = \"Rux\", Version = \"*\" }\n\
             Std = { Namespace = \"Rux\", Version = \"*\" }\n"
        ));
    }

    #[test]
    fn a_generated_manifest_with_dependencies_parses_and_resolves() {
        let rendered = manifest_for("import Std::Io\nFn Main() {}\n", &allowlist());
        let manifest = parse_manifest(&rendered).expect("generated manifest should parse");

        let ManifestKind::Package(package) = manifest.kind() else {
            panic!("generated manifest should be a package");
        };
        assert_eq!(package.name().as_str(), "Playground");
        assert_eq!(package.dependencies().len(), 1);
        assert!(package.dependencies().contains_key(&segment("Std")));
    }

    #[test]
    fn unknown_roots_are_omitted_so_the_compiler_reports_them() {
        let rendered = manifest_for("import Sneaky::Thing\nFn Main() {}\n", &allowlist());

        assert!(!rendered.contains("[Dependencies]"));
        assert!(!rendered.contains("Sneaky"));
    }

    #[test]
    fn dependency_spelling_comes_from_the_allowlist_not_the_source() {
        let rendered = manifest_for("import sTd::Io\nFn Main() {}\n", &allowlist());

        assert!(rendered.contains("Std = { Namespace = \"Rux\", Version = \"*\" }"));
        assert!(!rendered.contains("sTd"));
    }

    #[test]
    fn repeated_imports_of_one_root_yield_a_single_entry() {
        let source = "import Std::Io\nimport Std::Math\nimport Std::Time\nFn Main() {}\n";
        let rendered = manifest_for(source, &allowlist());

        assert_eq!(rendered.matches("Std = {").count(), 1);
    }

    #[test]
    fn rendering_is_deterministic_regardless_of_import_order() {
        let ordered = manifest_for(
            "import Http::A\nimport Std::B\nFn Main() {}\n",
            &allowlist(),
        );
        let reversed = manifest_for(
            "import Std::B\nimport Http::A\nFn Main() {}\n",
            &allowlist(),
        );

        assert_eq!(ordered, reversed);
    }

    #[test]
    fn only_well_formed_import_lines_contribute_a_root() {
        for source in [
            "imports Std::Io\n",        // not the `import` keyword
            "importStd::Io\n",          // no separating whitespace
            "import Std\n",             // no path separator
            "import Std:Io\n",          // single colon
            "import ::Io\n",            // empty root
            "let x = import Std::Io\n", // not in leading position
        ] {
            assert!(
                import_roots(source).is_empty(),
                "expected {source:?} to yield no roots"
            );
        }

        assert_eq!(import_roots("\t  import Std::Io\n").len(), 1);
    }

    #[test]
    fn an_empty_allowlist_never_renders_a_dependency_section() {
        let empty = PackageAllowlist::default();

        assert!(empty.is_empty());
        assert!(!manifest_for("import Std::Io\n", &empty).contains("[Dependencies]"));
    }
}
