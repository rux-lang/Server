use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};

use rux_manifest::{
    MANIFEST_MAX_BYTES, ManifestKind, ValidationProfile, parse_manifest_with_profile,
};
use zip::{CompressionMethod, ZipArchive};

use crate::error::{ArtifactError, ArtifactErrorCode};
use crate::model::ArtifactInspection;
use crate::path::{ValidatedPath, collision_key, validate_entry_path};
use crate::{
    ARTIFACT_MAX_BYTES, ARTIFACT_MAX_ENTRIES, ARTIFACT_MAX_EXPANDED_BYTES, ARTIFACT_MAX_FILE_BYTES,
    ARTIFACT_MAX_SOURCE_BYTES, ARTIFACT_MAX_TEXT_BYTES,
};

const UNIX_FILE_TYPE_MASK: u32 = 0o170_000;
const UNIX_DIRECTORY: u32 = 0o040_000;
const UNIX_REGULAR_FILE: u32 = 0o100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
}

#[derive(Clone, Debug)]
struct Entry {
    index: usize,
    path: String,
    collision_key: String,
    kind: EntryKind,
    declared_size: u64,
}

struct ManifestContext {
    manifest: rux_manifest::Manifest,
    bytes: Vec<u8>,
    readme_path: Option<String>,
    license_path: Option<String>,
}

/// Validates a complete `.ruxpkg` archive against its separately uploaded manifest.
///
/// The reader is rewound to byte zero. Entries are decompressed into bounded
/// per-file buffers only when their contents need to be retained.
///
/// # Errors
///
/// Returns a stable artifact diagnostic when the stream cannot be read or any
/// archive, layout, manifest, path, size, or text invariant is violated.
pub fn inspect_artifact<R: Read + Seek>(
    reader: R,
    expected_manifest: &[u8],
) -> Result<ArtifactInspection, ArtifactError> {
    let mut archive = open_archive(reader)?;
    let mut entries = inspect_directory(&mut archive)?;
    validate_collisions(&entries)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));

    let context = load_manifest(&mut archive, &entries, expected_manifest)?;
    inspect_contents(&mut archive, &entries, context)
}

fn open_archive<R: Read + Seek>(mut reader: R) -> Result<ZipArchive<R>, ArtifactError> {
    let archive_size = reader
        .seek(SeekFrom::End(0))
        .map_err(|error| io_error(&error))?;
    if archive_size > ARTIFACT_MAX_BYTES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::ArtifactTooLarge,
            None,
            format!("artifact exceeds {ARTIFACT_MAX_BYTES} bytes"),
        ));
    }
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&error))?;

    let archive = ZipArchive::new(reader).map_err(|error| zip_error(&error))?;
    if archive.len() > ARTIFACT_MAX_ENTRIES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::TooManyEntries,
            None,
            format!("archive contains more than {ARTIFACT_MAX_ENTRIES} entries"),
        ));
    }
    Ok(archive)
}

fn load_manifest<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    entries: &[Entry],
    expected_manifest: &[u8],
) -> Result<ManifestContext, ArtifactError> {
    let manifest_entry = entries
        .iter()
        .find(|entry| entry.path == "Rux.toml" && entry.kind == EntryKind::File)
        .ok_or_else(|| {
            ArtifactError::new(
                ArtifactErrorCode::MissingManifest,
                None,
                "archive must contain a regular Rux.toml at its root",
            )
        })?;
    let manifest_limit = u64::try_from(MANIFEST_MAX_BYTES).unwrap_or(u64::MAX);
    let manifest_bytes = read_entry(
        archive,
        manifest_entry,
        manifest_limit,
        ArtifactErrorCode::FileTooLarge,
        true,
    )?
    .1;

    if manifest_bytes != expected_manifest {
        return Err(ArtifactError::new(
            ArtifactErrorCode::ManifestMismatch,
            Some("Rux.toml".to_owned()),
            "embedded manifest does not exactly match the uploaded manifest",
        ));
    }
    let manifest_source = std::str::from_utf8(&manifest_bytes).map_err(|_| {
        ArtifactError::new(
            ArtifactErrorCode::InvalidManifest,
            Some("Rux.toml".to_owned()),
            "manifest is not UTF-8",
        )
    })?;
    let manifest = parse_manifest_with_profile(manifest_source, ValidationProfile::Publication)
        .map_err(ArtifactError::manifest)?;

    let ManifestKind::Package(package) = manifest.kind() else {
        unreachable!("publication validation only accepts packages");
    };
    let readme_path = package.readme_file().map(|path| path.as_str().to_owned());
    let license_path = package.license_file().map(|path| path.as_str().to_owned());

    Ok(ManifestContext {
        manifest,
        bytes: manifest_bytes,
        readme_path,
        license_path,
    })
}

fn inspect_contents<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    entries: &[Entry],
    context: ManifestContext,
) -> Result<ArtifactInspection, ArtifactError> {
    let ManifestContext {
        manifest,
        bytes: manifest_bytes,
        readme_path,
        license_path,
    } = context;
    let entries_by_path = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    validate_reference(&entries_by_path, readme_path.as_deref())?;
    validate_reference(&entries_by_path, license_path.as_deref())?;

    let source_count = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File && is_source(&entry.path))
        .count();
    if source_count == 0 {
        return Err(ArtifactError::new(
            ArtifactErrorCode::MissingSource,
            None,
            "archive must contain at least one Src/**/*.rux file",
        ));
    }

    let mut expanded_bytes = u64::try_from(manifest_bytes.len()).unwrap_or(u64::MAX);
    let mut source_line_count = 0_u64;
    let mut readme_file = None;
    let mut license_file = None;

    if readme_path.as_deref() == Some("Rux.toml") {
        readme_file = Some(decode_text(&manifest_bytes, "Rux.toml")?);
    }
    if license_path.as_deref() == Some("Rux.toml") {
        license_file = Some(decode_text(&manifest_bytes, "Rux.toml")?);
    }

    for entry in entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File && entry.path != "Rux.toml")
    {
        let source = is_source(&entry.path);
        let text = readme_path.as_deref() == Some(entry.path.as_str())
            || license_path.as_deref() == Some(entry.path.as_str());
        let (limit, limit_code) = if text {
            (ARTIFACT_MAX_TEXT_BYTES, ArtifactErrorCode::TextTooLarge)
        } else if source {
            (ARTIFACT_MAX_SOURCE_BYTES, ArtifactErrorCode::SourceTooLarge)
        } else {
            (ARTIFACT_MAX_FILE_BYTES, ArtifactErrorCode::FileTooLarge)
        };
        let (actual_size, bytes) = read_entry(archive, entry, limit, limit_code, source || text)?;
        expanded_bytes = expanded_bytes
            .checked_add(actual_size)
            .filter(|size| *size <= ARTIFACT_MAX_EXPANDED_BYTES)
            .ok_or_else(|| {
                ArtifactError::new(
                    ArtifactErrorCode::ExpandedSizeTooLarge,
                    Some(entry.path.clone()),
                    format!("expanded archive exceeds {ARTIFACT_MAX_EXPANDED_BYTES} bytes"),
                )
            })?;

        if source {
            std::str::from_utf8(&bytes).map_err(|_| {
                ArtifactError::new(
                    ArtifactErrorCode::InvalidUtf8Source,
                    Some(entry.path.clone()),
                    "Rux source is not UTF-8",
                )
            })?;
            source_line_count += physical_line_count(&bytes);
        }
        if text {
            let value = decode_text(&bytes, &entry.path)?;
            if readme_path.as_deref() == Some(entry.path.as_str()) {
                readme_file = Some(value.clone());
            }
            if license_path.as_deref() == Some(entry.path.as_str()) {
                license_file = Some(value);
            }
        }
    }

    Ok(ArtifactInspection {
        manifest,
        file_count: u32::try_from(
            entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::File)
                .count(),
        )
        .expect("entry limit fits u32"),
        expanded_bytes,
        source_file_count: u32::try_from(source_count).expect("entry limit fits u32"),
        source_line_count,
        readme_file,
        license_file,
    })
}

fn inspect_directory<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<Vec<Entry>, ArtifactError> {
    let mut entries = Vec::with_capacity(archive.len());
    let mut declared_expanded = 0_u64;

    for index in 0..archive.len() {
        let file = archive
            .by_index_raw(index)
            .map_err(|error| zip_error(&error))?;
        let ValidatedPath {
            path,
            collision_key,
            is_directory,
        } = validate_entry_path(file.name_raw())?;
        let entry_name = Some(path.clone());

        if file.encrypted() {
            return Err(ArtifactError::new(
                ArtifactErrorCode::EncryptedEntry,
                entry_name,
                "encrypted entries are not permitted",
            ));
        }
        if !matches!(
            file.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(ArtifactError::new(
                ArtifactErrorCode::UnsupportedCompression,
                entry_name,
                "only Stored and Deflate compression are permitted",
            ));
        }

        let kind = classify_entry(is_directory, file.unix_mode(), &path)?;
        let size = file.size();
        if kind == EntryKind::Directory && size != 0 {
            return Err(ArtifactError::new(
                ArtifactErrorCode::InvalidZip,
                Some(path),
                "directory entries must have zero uncompressed size",
            ));
        }
        if kind == EntryKind::File {
            let (limit, code) = if path == "Rux.toml" {
                (
                    u64::try_from(MANIFEST_MAX_BYTES).unwrap_or(u64::MAX),
                    ArtifactErrorCode::FileTooLarge,
                )
            } else if is_source(&path) {
                (ARTIFACT_MAX_SOURCE_BYTES, ArtifactErrorCode::SourceTooLarge)
            } else {
                (ARTIFACT_MAX_FILE_BYTES, ArtifactErrorCode::FileTooLarge)
            };
            if size > limit {
                return Err(ArtifactError::new(
                    code,
                    Some(path),
                    format!("entry exceeds {limit} uncompressed bytes"),
                ));
            }
            declared_expanded = declared_expanded
                .checked_add(size)
                .filter(|total| *total <= ARTIFACT_MAX_EXPANDED_BYTES)
                .ok_or_else(|| {
                    ArtifactError::new(
                        ArtifactErrorCode::ExpandedSizeTooLarge,
                        Some(path.clone()),
                        format!("expanded archive exceeds {ARTIFACT_MAX_EXPANDED_BYTES} bytes"),
                    )
                })?;
        }

        entries.push(Entry {
            index,
            path,
            collision_key,
            kind,
            declared_size: size,
        });
    }

    Ok(entries)
}

fn classify_entry(
    path_is_directory: bool,
    unix_mode: Option<u32>,
    path: &str,
) -> Result<EntryKind, ArtifactError> {
    let file_type = unix_mode.map_or(0, |mode| mode & UNIX_FILE_TYPE_MASK);
    match (path_is_directory, file_type) {
        (true, 0 | UNIX_DIRECTORY) => Ok(EntryKind::Directory),
        (false, 0 | UNIX_REGULAR_FILE) => Ok(EntryKind::File),
        _ => Err(ArtifactError::new(
            ArtifactErrorCode::UnsupportedEntryType,
            Some(path.to_owned()),
            "entry must be a regular file or consistently marked directory",
        )),
    }
}

fn validate_collisions(entries: &[Entry]) -> Result<(), ArtifactError> {
    let mut paths = BTreeMap::<&str, &str>::new();
    let mut component_spellings = BTreeMap::<String, String>::new();

    for entry in entries {
        if let Some(existing) = paths.insert(&entry.collision_key, &entry.path) {
            return Err(collision(&entry.path, existing));
        }

        let components = entry.path.split('/').collect::<Vec<_>>();
        for end in 1..=components.len() {
            let spelling = components[..end].join("/");
            let key = collision_key(&spelling);
            if let Some(existing) = component_spellings.insert(key, spelling.clone()) {
                if existing != spelling {
                    return Err(collision(&entry.path, &existing));
                }
            }
        }
    }

    let files = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| entry.collision_key.as_str())
        .collect::<BTreeSet<_>>();
    for entry in entries {
        let mut prefix = String::new();
        let component_count = entry.path.split('/').count();
        for (index, component) in entry.path.split('/').enumerate() {
            if index + 1 == component_count {
                break;
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let key = collision_key(&prefix);
            if files.contains(key.as_str()) {
                return Err(collision(&entry.path, &prefix));
            }
        }
    }

    Ok(())
}

fn validate_reference(
    entries: &BTreeMap<&str, &Entry>,
    path: Option<&str>,
) -> Result<(), ArtifactError> {
    let Some(path) = path else {
        return Ok(());
    };
    let entry = entries.get(path).ok_or_else(|| {
        ArtifactError::new(
            ArtifactErrorCode::MissingReferencedFile,
            Some(path.to_owned()),
            "manifest-referenced file is missing",
        )
    })?;
    if entry.kind != EntryKind::File {
        return Err(ArtifactError::new(
            ArtifactErrorCode::MissingReferencedFile,
            Some(path.to_owned()),
            "manifest reference must identify a regular file",
        ));
    }
    if entry.declared_size > ARTIFACT_MAX_TEXT_BYTES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::TextTooLarge,
            Some(path.to_owned()),
            format!("referenced text exceeds {ARTIFACT_MAX_TEXT_BYTES} bytes"),
        ));
    }
    Ok(())
}

fn read_entry<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    entry: &Entry,
    limit: u64,
    limit_code: ArtifactErrorCode,
    retain: bool,
) -> Result<(u64, Vec<u8>), ArtifactError> {
    let mut file = archive.by_index(entry.index).map_err(|error| {
        ArtifactError::new(
            ArtifactErrorCode::InvalidZip,
            Some(entry.path.clone()),
            error.to_string(),
        )
    })?;
    let mut bytes = if retain {
        Vec::with_capacity(usize::try_from(entry.declared_size).unwrap_or(0))
    } else {
        Vec::new()
    };
    let mut actual_size = 0_u64;
    let mut buffer = [0_u8; 8 * 1024];

    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            ArtifactError::new(
                ArtifactErrorCode::InvalidZip,
                Some(entry.path.clone()),
                error.to_string(),
            )
        })?;
        if read == 0 {
            break;
        }
        actual_size = actual_size
            .checked_add(u64::try_from(read).expect("buffer length fits u64"))
            .filter(|size| *size <= limit)
            .ok_or_else(|| {
                ArtifactError::new(
                    limit_code,
                    Some(entry.path.clone()),
                    format!("entry exceeds {limit} uncompressed bytes"),
                )
            })?;
        if retain {
            bytes.extend_from_slice(&buffer[..read]);
        }
    }

    if actual_size != entry.declared_size {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidZip,
            Some(entry.path.clone()),
            "decoded size does not match the central directory",
        ));
    }
    Ok((actual_size, bytes))
}

#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn is_source(path: &str) -> bool {
    // Package layout is case-sensitive; only the exact `.rux` spelling is source.
    path.strip_prefix("Src/")
        .is_some_and(|relative| !relative.is_empty() && relative.ends_with(".rux"))
}

fn physical_line_count(bytes: &[u8]) -> u64 {
    let newlines = bytes
        .iter()
        .fold(0_u64, |count, byte| count + u64::from(*byte == b'\n'));
    newlines + u64::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'))
}

fn decode_text(bytes: &[u8], path: &str) -> Result<String, ArtifactError> {
    std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| {
        ArtifactError::new(
            ArtifactErrorCode::InvalidUtf8Text,
            Some(path.to_owned()),
            "manifest-referenced text is not UTF-8",
        )
    })
}

fn collision(path: &str, existing: &str) -> ArtifactError {
    ArtifactError::new(
        ArtifactErrorCode::PathCollision,
        Some(path.to_owned()),
        format!("entry path collides with {existing}"),
    )
}

fn io_error(error: &std::io::Error) -> ArtifactError {
    ArtifactError::new(ArtifactErrorCode::Io, None, error.to_string())
}

fn zip_error(error: &zip::result::ZipError) -> ArtifactError {
    ArtifactError::new(ArtifactErrorCode::InvalidZip, None, error.to_string())
}
