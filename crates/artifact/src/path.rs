use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

use crate::{ARTIFACT_MAX_PATH_BYTES, ArtifactError, ArtifactErrorCode};

#[derive(Clone, Debug)]
pub(crate) struct ValidatedPath {
    pub(crate) path: String,
    pub(crate) collision_key: String,
    pub(crate) is_directory: bool,
}

pub(crate) fn validate_entry_path(raw: &[u8]) -> Result<ValidatedPath, ArtifactError> {
    let name = std::str::from_utf8(raw).map_err(|_| {
        ArtifactError::new(
            ArtifactErrorCode::InvalidEntryPath,
            None,
            "entry name is not UTF-8",
        )
    })?;
    let entry = (!name.is_empty()).then(|| name.to_owned());

    if name.len() > ARTIFACT_MAX_PATH_BYTES {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidEntryPath,
            entry,
            format!("entry path exceeds {ARTIFACT_MAX_PATH_BYTES} UTF-8 bytes"),
        ));
    }
    if name.is_empty() || name.starts_with('/') || name.starts_with('\\') {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidEntryPath,
            entry,
            "entry path must be a non-empty relative path",
        ));
    }
    if name.contains('\\') {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidEntryPath,
            entry,
            "entry paths must use forward slashes",
        ));
    }
    if !name.nfc().eq(name.chars()) {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidEntryPath,
            entry,
            "entry path must use Unicode NFC",
        ));
    }

    let is_directory = name.ends_with('/');
    let path = name.strip_suffix('/').unwrap_or(name);
    if path.is_empty() {
        return Err(ArtifactError::new(
            ArtifactErrorCode::InvalidEntryPath,
            entry,
            "entry path must contain a normal component",
        ));
    }

    for component in path.split('/') {
        validate_component(component, name)?;
    }

    Ok(ValidatedPath {
        path: path.to_owned(),
        collision_key: collision_key(path),
        is_directory,
    })
}

pub(crate) fn collision_key(path: &str) -> String {
    path.case_fold().nfc().collect()
}

fn validate_component(component: &str, entry: &str) -> Result<(), ArtifactError> {
    if component.is_empty() || component == "." || component == ".." {
        return Err(invalid(
            entry,
            "entry path contains an empty or dot component",
        ));
    }
    if component.ends_with(['.', ' ']) {
        return Err(invalid(
            entry,
            "entry path components cannot end with a dot or space",
        ));
    }
    if component
        .chars()
        .any(|character| character.is_control() || "<>:\"|?*".contains(character))
    {
        return Err(invalid(
            entry,
            "entry path contains a control or Windows-reserved character",
        ));
    }

    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    let numbered_device = upper
        .strip_prefix("COM")
        .or_else(|| upper.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered_device {
        return Err(invalid(
            entry,
            "entry path contains a Windows-reserved device name",
        ));
    }

    Ok(())
}

fn invalid(entry: &str, message: &'static str) -> ArtifactError {
    ArtifactError::new(
        ArtifactErrorCode::InvalidEntryPath,
        Some(entry.to_owned()),
        message,
    )
}
