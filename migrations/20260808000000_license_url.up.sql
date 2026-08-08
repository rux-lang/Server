-- `Package.LicenseFile` is gone from manifest schema Version 1 and `LicenseUrl`
-- replaces it, so a version no longer stores license text of its own: the
-- archive carries none and the terms are reached through the URL instead.
--
-- `license_expression` and `license_url` are independent. A version may declare
-- the SPDX expression, the URL, both, or neither, so the state constraint only
-- bounds whichever values are present.

ALTER TABLE package_versions
    DROP CONSTRAINT package_versions_license_state;

ALTER TABLE package_versions
    DROP COLUMN license_file_path,
    DROP COLUMN license_file_text,
    ADD COLUMN license_url TEXT;

ALTER TABLE package_versions
    ADD CONSTRAINT package_versions_license_state CHECK (
        (
            license_expression IS NULL
            OR octet_length(license_expression) BETWEEN 1 AND 512
        )
        AND (
            license_url IS NULL
            OR octet_length(license_url) BETWEEN 1 AND 2048
        )
    );
