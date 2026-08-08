-- Restores the mutually exclusive expression-or-file license state. License
-- text cannot be recovered from a URL, so versions that carried only a
-- `license_url` come back with no license recorded at all.

ALTER TABLE package_versions
    DROP CONSTRAINT package_versions_license_state;

ALTER TABLE package_versions
    DROP COLUMN license_url,
    ADD COLUMN license_file_path TEXT,
    ADD COLUMN license_file_text TEXT;

ALTER TABLE package_versions
    ADD CONSTRAINT package_versions_license_state CHECK (
        (license_expression IS NULL AND license_file_path IS NULL AND license_file_text IS NULL)
        OR (
            license_expression IS NOT NULL
            AND license_file_path IS NULL
            AND license_file_text IS NULL
            AND octet_length(license_expression) BETWEEN 1 AND 512
        )
        OR (
            license_expression IS NULL
            AND license_file_path IS NOT NULL
            AND license_file_text IS NOT NULL
            AND octet_length(license_file_path) BETWEEN 1 AND 2048
            AND octet_length(license_file_text) <= 1048576
        )
    );
