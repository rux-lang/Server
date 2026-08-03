# Rux Manifest v1

`Rux.toml` is a case-sensitive TOML 1.1 document. Schema-owned section names, field names, and enum values use PascalCase. Unversioned manifests and unknown fields are invalid; this prevents spelling mistakes from silently changing a build or publication.

## Package manifest

```toml
[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Namespace = "Rux"
Name = "Example"
Version = "1.2.3"
Type = "Source"
Description = "Example package"
Authors = ["Rux Contributors <info@rux-lang.dev>"]
Keywords = ["Example", "Registry"]
License = "MIT OR Apache-2.0"
Repository = "https://github.com/rux-lang/example"
Homepage = "https://example.dev"
Readme = "README.md"

[Dependencies]
Io = { Namespace = "Rux", Version = "^1.0" }
Json = { Namespace = "Acme", Package = "FastJson", Version = "2" }
LocalUtil = { Package = "Util", Path = "../Util" }

[Build]
Output = "Bin"

[Build.Defines]
Channel = "Nightly"

[Build.Debug]
Optimization = "None"
DebugInfo = true
DebugAssertions = true

[Build.Debug.Defines]
Tracing = true

[Build.Release]
Optimization = "Speed"
DebugInfo = false
DebugAssertions = false
Output = "Dist"

[Build.Release.Defines]
Tracing = false
```

`Manifest.Version` is an integer schema version. Version 1 requires `Manifest.MinRux` to be a strict semantic version with precedence greater than or equal to `0.4.0`. Build metadata does not affect that comparison.

A package requires `Name`, `Version`, and `Type`. `Namespace` is optional for local validation and required by the publication profile. Names and namespaces use registry identity-segment rules, and versions use the registry's strict SemVer rules. `Type` is exactly one of:

| Type      | Meaning                                                              |
| --------- | -------------------------------------------------------------------- |
| `Program` | A runnable executable with a `Main` entry point.                     |
| `Library` | A dynamic library linked by dependents and loaded at run time.       |
| `Source`  | Rux source files compiled directly into dependent packages.          |

Manifest v1 has no separate `SharedLibrary` type because `Library` already denotes one, and static archives are not a package type.

`License` is a strict SPDX expression. `LicenseFile` is a package-relative path alternative; a package cannot set both. Repository and homepage values are absolute HTTP or HTTPS URLs with a host and without credentials. Keywords use identity-segment syntax and cannot collide after normalization.

Each dependency key is the local import alias and each value is an inline table. A registry dependency requires `Namespace` and `Version`; `Package` defaults to the alias. A path dependency requires `Path`, may override `Package`, and cannot contain `Namespace` or `Version`. Path dependencies are valid locally but rejected by the publication profile.

## Workspace manifest

```toml
[Manifest]
Version = 1
MinRux = "0.4.0"

[Workspace]
Packages = [
  "Packages/Core",
  "Packages/Io",
]
```

A manifest contains exactly one of `Package` or `Workspace`. Workspace package paths are explicit, non-empty, relative paths below the workspace root; glob patterns and parent traversal are not supported. Workspace manifests cannot declare dependencies or build configuration and cannot be published.

## Validation profiles

Callers select validation policy; the selected profile is not stored in `Rux.toml`. Local validation accepts package and workspace manifests, allows a package to omit `Namespace`, and permits path dependencies. Publication validation accepts only package manifests with `Namespace` and rejects every path dependency. Publication failures use the same stable, source-located, deterministically ordered diagnostics as schema failures.

## Build modes

Only Debug and Release are supported. Their tables are optional overrides; an unknown table below `Build` is an error.

| Setting           | Debug default | Release default |
| ----------------- | ------------- | --------------- |
| `Optimization`    | `None`        | `Speed`         |
| `DebugInfo`       | `true`        | `false`         |
| `DebugAssertions` | `true`        | `false`         |

`Optimization` accepts `None`, `Size`, or `Speed`. `Build.Output` defaults to `Bin`. Without a mode-specific override, outputs resolve to `Bin/Debug` and `Bin/Release` (or the selected base followed by the mode name). A mode-specific `Output` is the complete output directory.

Shared `Build.Defines` are overlaid by the selected mode's `Defines`. Define values may be strings, Booleans, or signed 64-bit integers. Define names are 1-64 byte ASCII identifiers beginning with a letter or underscore.

## Paths and limits

Manifest paths are UTF-8, relative, `/`-separated paths. Backslashes, roots, empty components, and `.` components are invalid. README, license, and workspace paths also reject `..`. Dependency and output paths may start with one or more `..` components, but parent traversal cannot appear after a normal component.

All limits count UTF-8 bytes:

| Resource                          |    Limit |
| --------------------------------- | -------: |
| Manifest source                   |   65,536 |
| Dependencies / workspace packages | 256 each |
| Defines per table                 |      128 |
| Authors / keywords                |  32 each |
| Description                       |    2,048 |
| Author                            |      256 |
| URL or path                       |    2,048 |
| SPDX expression / version range   |      512 |
| Semantic version                  |      256 |
| Define string                     |    1,024 |
| Keyword or define name            |       64 |

## Diagnostics

The parser returns one syntax or size error when it cannot continue. Otherwise it accumulates independent schema failures and orders them by source byte span, stable error code, and field path. Every diagnostic contains a stable snake_case code, structured field path, human-readable message, zero-based end-exclusive byte span, and one-based line and Unicode-scalar column.
