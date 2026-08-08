use rux_manifest::Manifest;

/// Validated package contents and bounded metadata derived from a `.ruxpkg`.
#[derive(Clone, Debug)]
pub struct ArtifactInspection {
    pub(crate) manifest: Manifest,
    pub(crate) file_count: u32,
    pub(crate) expanded_bytes: u64,
    pub(crate) source_file_count: u32,
    pub(crate) source_line_count: u64,
    pub(crate) readme_file: Option<String>,
    pub(crate) license_file: Option<String>,
}

impl ArtifactInspection {
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    #[must_use]
    pub const fn file_count(&self) -> u32 {
        self.file_count
    }

    #[must_use]
    pub const fn expanded_bytes(&self) -> u64 {
        self.expanded_bytes
    }

    #[must_use]
    pub const fn source_file_count(&self) -> u32 {
        self.source_file_count
    }

    #[must_use]
    pub const fn source_line_count(&self) -> u64 {
        self.source_line_count
    }

    #[must_use]
    pub fn readme_file(&self) -> Option<&str> {
        self.readme_file.as_deref()
    }

    #[must_use]
    pub fn license_file(&self) -> Option<&str> {
        self.license_file.as_deref()
    }
}
