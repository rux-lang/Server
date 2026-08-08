-- `Package.LicenseUrl` is gone again. Manifest schema Version 1 names the terms
-- with an SPDX expression in `License` and carries their text as an archive
-- entry named by `LicenseFile`. A URL pointed outside the archive, so its
-- target was never covered by the release checksum and could change or rot
-- after publication; a stored file cannot.
--
-- `License` and `LicenseFile` are **independent**, not mutually exclusive as
-- the initial schema had them. The expression is what machines read and the
-- file is what people read, so the common case declares both, and the state
-- constraint now bounds each side only when it is present.
--
-- `Readme` becomes `ReadmeFile` in the same schema revision, which makes the
-- `File` suffix mean "an archive path" everywhere it appears. The columns
-- follow. Renaming carries the generated `search_vector` expression and the
-- readme CHECK along with it: PostgreSQL stores both as parse trees keyed by
-- column number rather than by name.

ALTER TABLE package_versions
    DROP CONSTRAINT package_versions_license_state;

ALTER TABLE package_versions
    DROP COLUMN license_url,
    ADD COLUMN license_file_path TEXT,
    ADD COLUMN license_file_text TEXT;

ALTER TABLE package_versions
    ADD CONSTRAINT package_versions_license_state CHECK (
        (
            license_expression IS NULL
            OR octet_length(license_expression) BETWEEN 1 AND 512
        )
        AND (
            (license_file_path IS NULL AND license_file_text IS NULL)
            OR (
                license_file_path IS NOT NULL
                AND license_file_text IS NOT NULL
                AND octet_length(license_file_path) BETWEEN 1 AND 2048
                AND octet_length(license_file_text) <= 1048576
            )
        )
    );

ALTER TABLE package_versions RENAME COLUMN readme_path TO readme_file_path;

ALTER TABLE package_versions RENAME COLUMN readme_text TO readme_file_text;

ALTER TABLE package_versions
    RENAME CONSTRAINT package_versions_readme_state TO package_versions_readme_file_state;
