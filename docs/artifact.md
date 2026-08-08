# Rux Package Artifact v1

A published Rux package uses the `.ruxpkg` extension. Its bytes are a ZIP archive whose contents satisfy this contract. The custom extension identifies the archive as a registry package; ordinary ZIP tools can still inspect it.

Artifact v1 is tied to the embedded `Rux.toml` manifest schema v1. ZIP entry order, timestamps, comments, permission bits other than file type, and harmless extra fields are not canonicalized and do not affect validity.

## Layout

Every artifact contains a manifest and at least one Rux source below `Src/`:

```text
Example.ruxpkg
├── Rux.toml
├── Src/
│   ├── Main.rux
│   └── Nested/Other.rux
├── README.md       # when referenced by Package.Readme
└── Assets/         # optional additional regular files
```

`Rux.toml` is an exact-case regular file at archive root. A containing package directory such as `Example/Rux.toml` is invalid. The archive must contain at least one exact-case `Src/**/*.rux` regular file. Files with another extension, another extension spelling, or outside `Src/` may be included but do not count as Rux sources.

Directory entries are optional because parent directories may be implicit. Additional safe regular files are allowed. Symlinks, devices, FIFOs, sockets, and other special entries are invalid. Entries may use only the ZIP Stored or Deflate compression methods and cannot be encrypted.

## Manifest and referenced text

Publication supplies `Rux.toml` separately from the `.ruxpkg`. The root manifest in the archive must match those uploaded bytes exactly, including comments, whitespace, and line endings. The matching source must be UTF-8 and pass the manifest publication validation profile.

When `Package.Readme` is present, its exact path must identify a regular file in the archive, and that file must be UTF-8. Its bounded text is returned by artifact inspection for later publication metadata. Neither `Package.License` nor `Package.LicenseUrl` references an archive entry, so an archive carries no license text of its own; a `LICENSE` file included beside the sources is an ordinary additional regular file.

## Portable entry paths

Entry names are UTF-8, Unicode NFC, relative, and `/`-separated. A directory entry has one trailing `/`; that marker is not part of its logical path. Each logical path is at most 2,048 UTF-8 bytes.

The following are invalid:

- absolute, drive-qualified, or UNC paths;
- backslashes, NULs, control characters, empty components, `.` or `..`;
- Windows-reserved characters, device names, or components ending in a dot or space;
- duplicate logical paths, Unicode case-fold collisions, inconsistent component spelling, or a regular file used as another entry's directory.

These rules make one archive extract to the same logical tree on supported case-sensitive and case-insensitive platforms. Validation does not extract any entry.

## Limits

Limits are inclusive and use binary mebibytes:

| Resource                           |        Limit |
| ---------------------------------- | -----------: |
| Complete `.ruxpkg`                 |        5 MiB |
| Combined expanded regular files    |       10 MiB |
| ZIP entries, including directories |        1,024 |
| One regular file                   |        2 MiB |
| One `Src/**/*.rux` source          |        2 MiB |
| Referenced README                  |        1 MiB |
| Embedded `Rux.toml`                | 65,536 bytes |
| Entry path                         |  2,048 bytes |

Central-directory sizes are checked before decompression. Every regular file is then read to its end with bounded counters so actual expanded sizes and CRCs are verified. The combined limit includes the manifest, sources, referenced text, and all additional regular files; directories contribute no expanded bytes.

## Source metrics

Every source must be valid UTF-8. Source-file count includes only regular files whose exact path matches `Src/**/*.rux`.

Source-line count is a physical metric summed across those files. Each LF byte ends one line, so CRLF ends one line. A non-empty file whose final byte is not LF contributes one final unterminated line. An empty file contributes zero lines, and a lone CR is source text rather than a line ending.

## Inspection API

The `rux-artifact` crate exposes `inspect_artifact(reader, expected_manifest)`. It accepts a `Read + Seek` archive stream, performs publication manifest validation and bounded entry inspection, and returns the parsed manifest, archive statistics, source metrics, and optional referenced text.

Failures expose stable snake_case artifact error codes plus an optional entry path. Invalid embedded manifests retain the manifest parser's deterministic, source-located diagnostics. The inspector performs no extraction, temporary file creation, persistence, HTTP mapping, or object-storage operation.
