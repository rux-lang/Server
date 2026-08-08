-- Returns the readme columns to their unsuffixed names and restores the
-- `license_url` column with the independent bounds it carried.
--
-- A URL cannot be reconstructed from stored licence text, so a version that
-- recorded only a `license_file` reverts to no license URL at all. Its
-- `license_expression` survives untouched.

ALTER TABLE package_versions
    RENAME CONSTRAINT package_versions_readme_file_state TO package_versions_readme_state;

ALTER TABLE package_versions RENAME COLUMN readme_file_text TO readme_text;

ALTER TABLE package_versions RENAME COLUMN readme_file_path TO readme_path;

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
