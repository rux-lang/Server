use std::io::{Cursor, Write};

use rux_artifact::{
    ARTIFACT_MAX_BYTES, ARTIFACT_MAX_ENTRIES, ARTIFACT_MAX_EXPANDED_BYTES, ARTIFACT_MAX_FILE_BYTES,
    ARTIFACT_MAX_PATH_BYTES, ARTIFACT_MAX_SOURCE_BYTES, ARTIFACT_MAX_TEXT_BYTES, ArtifactErrorCode,
    inspect_artifact,
};
use rux_manifest::{MANIFEST_MAX_BYTES, ManifestErrorCode};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

const MANIFEST: &str = r#"[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Namespace = "Rux"
Name = "Example"
Version = "1.2.3"
Type = "SourceLibrary"
License = "MIT"
LicenseFile = "LICENSE"
ReadmeFile = "README.md"
"#;

const MINIMAL_MANIFEST: &str = r#"[Manifest]
Version = 1
MinRux = "0.4.0"

[Package]
Namespace = "Rux"
Name = "Example"
Version = "1.2.3"
Type = "SourceLibrary"
"#;

#[test]
fn valid_archive_returns_manifest_text_and_source_statistics() {
    let source_one = "first\nsecond\r\nthird";
    let source_two = "λ\n";
    let readme = "# Example\n";
    let license = "Example license";
    let extra = [0_u8, 1, 2, 3];
    let archive = package(
        MANIFEST,
        &[
            directory("Src/"),
            deflated("Src/Main.rux", source_one.as_bytes()),
            stored("Src/Nested/Other.rux", source_two.as_bytes()),
            stored("README.md", readme.as_bytes()),
            deflated("LICENSE", license.as_bytes()),
            stored("assets/data.bin", &extra),
        ],
    );

    let inspected = inspect_artifact(Cursor::new(archive), MANIFEST.as_bytes())
        .expect("valid artifact should inspect");

    assert_eq!(inspected.file_count(), 6);
    assert_eq!(inspected.source_file_count(), 2);
    assert_eq!(inspected.source_line_count(), 4);
    assert_eq!(inspected.readme_file(), Some(readme));
    assert_eq!(inspected.license_file(), Some(license));
    assert_eq!(
        inspected.expanded_bytes(),
        u64::try_from(
            MANIFEST.len()
                + source_one.len()
                + source_two.len()
                + readme.len()
                + license.len()
                + extra.len()
        )
        .expect("fixture size fits u64")
    );

    let rux_manifest::ManifestKind::Package(package) = inspected.manifest().kind() else {
        panic!("publication artifact should contain a package manifest");
    };
    assert_eq!(
        package.namespace().map(ToString::to_string).as_deref(),
        Some("Rux")
    );
}

#[test]
fn source_line_count_uses_physical_lf_lines() {
    for (source, expected) in [
        (b"".as_slice(), 0),
        (b"one".as_slice(), 1),
        (b"one\n".as_slice(), 1),
        (b"one\r\ntwo".as_slice(), 2),
        (b"one\rtwo".as_slice(), 1),
        (b"\n\n".as_slice(), 2),
    ] {
        let archive = package(MINIMAL_MANIFEST, &[stored("Src/Main.rux", source)]);
        let inspected = inspect_artifact(Cursor::new(archive), MINIMAL_MANIFEST.as_bytes())
            .expect("line-count fixture should inspect");
        assert_eq!(
            inspected.source_line_count(),
            expected,
            "source: {source:?}"
        );
    }
}

#[test]
fn archive_and_entry_limits_are_enforced() {
    let oversized_archive = vec![0_u8; usize::try_from(ARTIFACT_MAX_BYTES + 1).unwrap()];
    assert_code(
        oversized_archive,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::ArtifactTooLarge,
    );

    let too_many = (0..=ARTIFACT_MAX_ENTRIES)
        .map(|index| stored_owned(format!("file-{index}"), Vec::new()))
        .collect::<Vec<_>>();
    assert_code(
        package_owned(MINIMAL_MANIFEST, &too_many),
        MINIMAL_MANIFEST,
        ArtifactErrorCode::TooManyEntries,
    );

    let oversized_file = vec![b'x'; usize::try_from(ARTIFACT_MAX_FILE_BYTES + 1).unwrap()];
    assert_code(
        package_owned(
            MINIMAL_MANIFEST,
            &[
                deflated_owned("Src/Main.rux", b"source".to_vec()),
                deflated_owned("large.bin", oversized_file),
            ],
        ),
        MINIMAL_MANIFEST,
        ArtifactErrorCode::FileTooLarge,
    );
}

#[test]
fn exact_archive_and_entry_boundaries_are_accepted() {
    let first = vec![b'a'; usize::try_from(ARTIFACT_MAX_FILE_BYTES).unwrap()];
    let second = vec![b'b'; usize::try_from(ARTIFACT_MAX_FILE_BYTES).unwrap()];
    let base_entries = [
        stored("Src/Main.rux", b"source"),
        stored_owned("first.bin", first),
        stored_owned("second.bin", second),
        stored_owned("padding.bin", Vec::new()),
    ];
    let base = package_owned(MINIMAL_MANIFEST, &base_entries);
    let padding_size = usize::try_from(ARTIFACT_MAX_BYTES).unwrap() - base.len();
    assert!(u64::try_from(padding_size).unwrap() <= ARTIFACT_MAX_FILE_BYTES);
    let archive = package_owned(
        MINIMAL_MANIFEST,
        &[
            base_entries[0].clone(),
            base_entries[1].clone(),
            base_entries[2].clone(),
            stored_owned("padding.bin", vec![0_u8; padding_size]),
        ],
    );
    assert_eq!(u64::try_from(archive.len()).unwrap(), ARTIFACT_MAX_BYTES);
    inspect_artifact(Cursor::new(archive.clone()), MINIMAL_MANIFEST.as_bytes())
        .expect("exact artifact byte limit should be accepted");
    let mut too_large = archive;
    too_large.push(0);
    assert_code(
        too_large,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::ArtifactTooLarge,
    );

    let exact_entries = (0..ARTIFACT_MAX_ENTRIES - 2)
        .map(|index| stored_owned(format!("file-{index}"), Vec::new()))
        .chain(std::iter::once(stored_owned(
            "Src/Main.rux",
            b"source".to_vec(),
        )))
        .collect::<Vec<_>>();
    let exact_archive = package_owned(MINIMAL_MANIFEST, &exact_entries);
    inspect_artifact(Cursor::new(exact_archive), MINIMAL_MANIFEST.as_bytes())
        .expect("exact entry limit should be accepted");

    let mut over_entries = exact_entries;
    over_entries.push(stored_owned("one-more", Vec::new()));
    assert_code(
        package_owned(MINIMAL_MANIFEST, &over_entries),
        MINIMAL_MANIFEST,
        ArtifactErrorCode::TooManyEntries,
    );
}

#[test]
fn exact_content_and_path_boundaries_are_accepted() {
    let two_mib = vec![b'a'; usize::try_from(ARTIFACT_MAX_FILE_BYTES).unwrap()];
    let manifest_size = u64::try_from(MINIMAL_MANIFEST.len()).unwrap();
    let final_size = ARTIFACT_MAX_FILE_BYTES - manifest_size;
    let exact_entries = [
        deflated_owned("Src/Main.rux", two_mib.clone()),
        deflated_owned("one.bin", two_mib.clone()),
        deflated_owned("two.bin", two_mib.clone()),
        deflated_owned("three.bin", two_mib.clone()),
        deflated_owned("four.bin", vec![0_u8; usize::try_from(final_size).unwrap()]),
    ];
    let exact_expanded = package_owned(MINIMAL_MANIFEST, &exact_entries);
    let inspected = inspect_artifact(Cursor::new(exact_expanded), MINIMAL_MANIFEST.as_bytes())
        .expect("exact expanded limit should be accepted");
    assert_eq!(inspected.expanded_bytes(), ARTIFACT_MAX_EXPANDED_BYTES);
    let mut over_expanded = exact_entries.to_vec();
    over_expanded[4] = deflated_owned(
        "four.bin",
        vec![0_u8; usize::try_from(final_size + 1).unwrap()],
    );
    assert_code(
        package_owned(MINIMAL_MANIFEST, &over_expanded),
        MINIMAL_MANIFEST,
        ArtifactErrorCode::ExpandedSizeTooLarge,
    );

    let exact_source = package_owned(MINIMAL_MANIFEST, &[deflated_owned("Src/Main.rux", two_mib)]);
    inspect_artifact(Cursor::new(exact_source), MINIMAL_MANIFEST.as_bytes())
        .expect("exact source and regular-file limit should be accepted");
    let oversized_source = vec![b'a'; usize::try_from(ARTIFACT_MAX_SOURCE_BYTES + 1).unwrap()];
    assert_code(
        package_owned(
            MINIMAL_MANIFEST,
            &[deflated_owned("Src/Main.rux", oversized_source)],
        ),
        MINIMAL_MANIFEST,
        ArtifactErrorCode::SourceTooLarge,
    );

    let exact_text = vec![b'r'; usize::try_from(ARTIFACT_MAX_TEXT_BYTES).unwrap()];
    let text_archive = package_owned(
        MANIFEST,
        &[
            stored_owned("Src/Main.rux", b"source".to_vec()),
            deflated_owned("README.md", exact_text),
            stored_owned("LICENSE", b"license".to_vec()),
        ],
    );
    inspect_artifact(Cursor::new(text_archive), MANIFEST.as_bytes())
        .expect("exact referenced-text limit should be accepted");

    let exact_manifest = padded_manifest(MANIFEST_MAX_BYTES);
    let manifest_archive = package(
        &exact_manifest,
        &[
            stored("Src/Main.rux", b"source"),
            stored("README.md", b"readme"),
            stored("LICENSE", b"license"),
        ],
    );
    inspect_artifact(Cursor::new(manifest_archive), exact_manifest.as_bytes())
        .expect("exact manifest limit should be accepted");
    let oversized_manifest = padded_manifest(MANIFEST_MAX_BYTES + 1);
    assert_code(
        package(
            &oversized_manifest,
            &[
                stored("Src/Main.rux", b"source"),
                stored("README.md", b"readme"),
                stored("LICENSE", b"license"),
            ],
        ),
        &oversized_manifest,
        ArtifactErrorCode::FileTooLarge,
    );

    let prefix = "assets/";
    let exact_path = format!(
        "{prefix}{}",
        "a".repeat(ARTIFACT_MAX_PATH_BYTES - prefix.len())
    );
    let path_archive = package_owned(
        MINIMAL_MANIFEST,
        &[
            stored_owned("Src/Main.rux", b"source".to_vec()),
            stored_owned(exact_path, Vec::new()),
        ],
    );
    inspect_artifact(Cursor::new(path_archive), MINIMAL_MANIFEST.as_bytes())
        .expect("exact entry-path limit should be accepted");
    let oversized_path = format!(
        "{prefix}{}",
        "a".repeat(ARTIFACT_MAX_PATH_BYTES + 1 - prefix.len())
    );
    assert_code(
        package_owned(
            MINIMAL_MANIFEST,
            &[
                stored_owned("Src/Main.rux", b"source".to_vec()),
                stored_owned(oversized_path, Vec::new()),
            ],
        ),
        MINIMAL_MANIFEST,
        ArtifactErrorCode::InvalidEntryPath,
    );
}

#[test]
fn expanded_total_is_enforced_independently_of_zip_size() {
    let chunk = vec![0_u8; usize::try_from(ARTIFACT_MAX_FILE_BYTES).unwrap()];
    let mut entries = vec![deflated_owned("Src/Main.rux", chunk.clone())];
    for index in 0..5 {
        entries.push(deflated_owned(format!("data-{index}.bin"), chunk.clone()));
    }
    let archive = package_owned(MINIMAL_MANIFEST, &entries);
    assert!(
        u64::try_from(archive.len()).unwrap() < ARTIFACT_MAX_BYTES,
        "fixture must exercise expansion rather than upload size"
    );
    assert_code(
        archive,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::ExpandedSizeTooLarge,
    );
    assert_eq!(ARTIFACT_MAX_EXPANDED_BYTES, 10 * 1024 * 1024);
}

#[test]
fn referenced_text_has_a_narrower_limit() {
    let readme = vec![b'a'; usize::try_from(ARTIFACT_MAX_TEXT_BYTES + 1).unwrap()];
    assert_code(
        package_owned(
            MANIFEST,
            &[
                stored_owned("Src/Main.rux", b"source".to_vec()),
                deflated_owned("README.md", readme),
                stored_owned("LICENSE", b"license".to_vec()),
            ],
        ),
        MANIFEST,
        ArtifactErrorCode::TextTooLarge,
    );
}

#[test]
fn manifest_must_be_rooted_exact_and_publishable() {
    let missing = archive(&[stored("Src/Main.rux", b"source")]);
    assert_code(
        missing,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::MissingManifest,
    );

    let wrapped = archive(&[
        stored("Example/Rux.toml", MINIMAL_MANIFEST.as_bytes()),
        stored("Example/Src/Main.rux", b"source"),
    ]);
    assert_code(
        wrapped,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::MissingManifest,
    );

    let mismatch = package(MINIMAL_MANIFEST, &[stored("Src/Main.rux", b"source")]);
    assert_code(mismatch, "different", ArtifactErrorCode::ManifestMismatch);

    let local_only = MINIMAL_MANIFEST.replace("Namespace = \"Rux\"\n", "");
    let invalid = package(&local_only, &[stored("Src/Main.rux", b"source")]);
    let error = inspect_artifact(Cursor::new(invalid), local_only.as_bytes())
        .expect_err("local-only manifest must not publish");
    assert_eq!(error.code(), ArtifactErrorCode::InvalidManifest);
    assert_eq!(
        error
            .manifest_errors()
            .expect("manifest diagnostics should be retained")
            .as_slice()[0]
            .code(),
        ManifestErrorCode::MissingField
    );
}

#[test]
fn source_and_referenced_text_are_required_and_utf8() {
    let no_source = package(
        MANIFEST,
        &[
            stored("README.md", b"readme"),
            stored("LICENSE", b"license"),
        ],
    );
    assert_code(no_source, MANIFEST, ArtifactErrorCode::MissingSource);

    let missing_readme = package(
        MANIFEST,
        &[
            stored("Src/Main.rux", b"source"),
            stored("LICENSE", b"license"),
        ],
    );
    assert_code(
        missing_readme,
        MANIFEST,
        ArtifactErrorCode::MissingReferencedFile,
    );

    let invalid_source = package(
        MANIFEST,
        &[
            stored("Src/Main.rux", &[0xff]),
            stored("README.md", b"readme"),
            stored("LICENSE", b"license"),
        ],
    );
    assert_code(
        invalid_source,
        MANIFEST,
        ArtifactErrorCode::InvalidUtf8Source,
    );

    let invalid_readme = package(
        MANIFEST,
        &[
            stored("Src/Main.rux", b"source"),
            stored("README.md", &[0xff]),
            stored("LICENSE", b"license"),
        ],
    );
    assert_code(invalid_readme, MANIFEST, ArtifactErrorCode::InvalidUtf8Text);
}

#[test]
fn unsafe_and_nonportable_paths_are_rejected() {
    for path in [
        "../escape",
        "/absolute",
        "C:/drive",
        r"Src\Main.rux",
        "Src//Main.rux",
        "Src/./Main.rux",
        "Src/CON.rux",
        "Src/trailing. ",
        "Src/Cafe\u{301}.rux",
    ] {
        let archive = package(MINIMAL_MANIFEST, &[stored(path, b"source")]);
        assert_code(
            archive,
            MINIMAL_MANIFEST,
            ArtifactErrorCode::InvalidEntryPath,
        );
    }
}

#[test]
fn path_collisions_and_file_prefixes_are_rejected() {
    let case_collision = package(
        MINIMAL_MANIFEST,
        &[
            stored("Src/Main.rux", b"source"),
            stored("src/main.RUX", b"other"),
        ],
    );
    assert_code(
        case_collision,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::PathCollision,
    );

    let file_prefix = package(
        MINIMAL_MANIFEST,
        &[
            stored("Src/Main.rux", b"source"),
            stored("assets", b"file"),
            stored("assets/data.bin", b"nested"),
        ],
    );
    assert_code(
        file_prefix,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::PathCollision,
    );
}

#[test]
fn raw_non_utf8_entry_name_is_rejected() {
    let mut bytes = package(MINIMAL_MANIFEST, &[stored("Src/Main.rux", b"source")]);
    patch_filename_byte(&mut bytes, "Src/Main.rux", 0xff);
    assert_code(bytes, MINIMAL_MANIFEST, ArtifactErrorCode::InvalidEntryPath);
}

#[test]
fn encrypted_and_unsupported_compression_flags_are_rejected() {
    let base = package(MINIMAL_MANIFEST, &[stored("Src/Main.rux", b"source")]);

    let mut encrypted = base.clone();
    patch_entry_flags(&mut encrypted, "Src/Main.rux", 0x0001);
    assert_code(
        encrypted,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::EncryptedEntry,
    );

    let mut unsupported = base;
    patch_compression_method(&mut unsupported, "Src/Main.rux", 12);
    assert_code(
        unsupported,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::UnsupportedCompression,
    );
}

#[test]
fn symlinks_special_files_and_corrupt_content_are_rejected() {
    let base = package(MINIMAL_MANIFEST, &[stored("Src/Main.rux", b"source")]);

    let mut symlink = base.clone();
    patch_unix_mode(&mut symlink, "Src/Main.rux", 0o120_777);
    assert_code(
        symlink,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::UnsupportedEntryType,
    );

    let mut fifo = base.clone();
    patch_unix_mode(&mut fifo, "Src/Main.rux", 0o010_644);
    assert_code(
        fifo,
        MINIMAL_MANIFEST,
        ArtifactErrorCode::UnsupportedEntryType,
    );

    let mut corrupt = base;
    patch_crc(&mut corrupt, "Src/Main.rux");
    assert_code(corrupt, MINIMAL_MANIFEST, ArtifactErrorCode::InvalidZip);
}

#[derive(Clone)]
struct FixtureEntry {
    name: String,
    bytes: Vec<u8>,
    compression: CompressionMethod,
    directory: bool,
}

fn stored(name: impl Into<String>, bytes: &[u8]) -> FixtureEntry {
    stored_owned(name, bytes.to_vec())
}

fn stored_owned(name: impl Into<String>, bytes: Vec<u8>) -> FixtureEntry {
    FixtureEntry {
        name: name.into(),
        bytes,
        compression: CompressionMethod::Stored,
        directory: false,
    }
}

fn deflated(name: impl Into<String>, bytes: &[u8]) -> FixtureEntry {
    deflated_owned(name, bytes.to_vec())
}

fn deflated_owned(name: impl Into<String>, bytes: Vec<u8>) -> FixtureEntry {
    FixtureEntry {
        name: name.into(),
        bytes,
        compression: CompressionMethod::Deflated,
        directory: false,
    }
}

fn directory(name: impl Into<String>) -> FixtureEntry {
    FixtureEntry {
        name: name.into(),
        bytes: Vec::new(),
        compression: CompressionMethod::Stored,
        directory: true,
    }
}

fn package(manifest: &str, entries: &[FixtureEntry]) -> Vec<u8> {
    package_owned(manifest, entries)
}

fn package_owned(manifest: &str, entries: &[FixtureEntry]) -> Vec<u8> {
    let mut all = vec![stored("Rux.toml", manifest.as_bytes())];
    all.extend_from_slice(entries);
    archive(&all)
}

fn archive(entries: &[FixtureEntry]) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        let options = SimpleFileOptions::default().compression_method(entry.compression);
        if entry.directory {
            writer
                .add_directory(&entry.name, options)
                .expect("fixture directory should write");
        } else {
            writer
                .start_file(&entry.name, options)
                .expect("fixture file should start");
            writer
                .write_all(&entry.bytes)
                .expect("fixture bytes should write");
        }
    }
    writer
        .finish()
        .expect("fixture archive should finish")
        .into_inner()
}

fn assert_code(archive: Vec<u8>, manifest: &str, expected: ArtifactErrorCode) {
    let error = inspect_artifact(Cursor::new(archive), manifest.as_bytes())
        .expect_err("fixture should be rejected");
    assert_eq!(error.code(), expected, "{error}");
}

fn patch_filename_byte(bytes: &mut [u8], name: &str, replacement: u8) {
    for offset in entry_header_offsets(bytes, name) {
        let name_offset = if bytes[offset + 2] == 3 { 30 } else { 46 };
        bytes[offset + name_offset] = replacement;
    }
}

fn patch_entry_flags(bytes: &mut [u8], name: &str, flags: u16) {
    for offset in entry_header_offsets(bytes, name) {
        let flag_offset = if bytes[offset + 2] == 3 { 6 } else { 8 };
        bytes[offset + flag_offset..offset + flag_offset + 2].copy_from_slice(&flags.to_le_bytes());
    }
}

fn patch_compression_method(bytes: &mut [u8], name: &str, method: u16) {
    for offset in entry_header_offsets(bytes, name) {
        let method_offset = if bytes[offset + 2] == 3 { 8 } else { 10 };
        bytes[offset + method_offset..offset + method_offset + 2]
            .copy_from_slice(&method.to_le_bytes());
    }
}

fn entry_header_offsets(bytes: &[u8], name: &str) -> Vec<usize> {
    let signatures = [[0x50, 0x4b, 0x03, 0x04], [0x50, 0x4b, 0x01, 0x02]];
    let mut offsets = Vec::new();
    for signature in signatures {
        for offset in 0..bytes.len().saturating_sub(4) {
            if bytes[offset..offset + 4] != signature {
                continue;
            }
            let name_offset = if signature[2] == 3 { 30 } else { 46 };
            let length_offset = if signature[2] == 3 { 26 } else { 28 };
            let name_length = usize::from(u16::from_le_bytes([
                bytes[offset + length_offset],
                bytes[offset + length_offset + 1],
            ]));
            if bytes.get(offset + name_offset..offset + name_offset + name_length)
                == Some(name.as_bytes())
            {
                offsets.push(offset);
            }
        }
    }
    assert_eq!(offsets.len(), 2, "fixture entry should have two headers");
    offsets
}

fn patch_unix_mode(bytes: &mut [u8], name: &str, mode: u32) {
    let offsets = entry_header_offsets(bytes, name);
    let central = offsets
        .into_iter()
        .find(|offset| bytes[*offset + 2] == 1)
        .expect("fixture should have a central header");
    bytes[central + 5] = 3;
    bytes[central + 38..central + 42].copy_from_slice(&(mode << 16).to_le_bytes());
}

fn patch_crc(bytes: &mut [u8], name: &str) {
    let offsets = entry_header_offsets(bytes, name);
    let central = offsets
        .into_iter()
        .find(|offset| bytes[*offset + 2] == 1)
        .expect("fixture should have a central header");
    bytes[central + 16] ^= 0xff;
}

fn padded_manifest(size: usize) -> String {
    assert!(size > MANIFEST.len() + 2);
    let mut manifest = MANIFEST.to_owned();
    manifest.push('#');
    manifest.push_str(&"p".repeat(size - MANIFEST.len() - 2));
    manifest.push('\n');
    assert_eq!(manifest.len(), size);
    manifest
}
