DROP TABLE blocked_identities;
DROP TABLE download_events;
DROP TABLE audit_records;
DROP TABLE api_token_scopes;
DROP TABLE api_tokens;
DROP TABLE dependencies;
DROP TABLE package_version_keywords;
DROP TABLE package_version_authors;
DROP TABLE package_versions;
DROP TABLE packages;
DROP TABLE namespace_invitations;
DROP TABLE namespace_owners;
DROP TABLE namespaces;
DROP TABLE sessions;
DROP TABLE users;

DROP FUNCTION enforce_package_version_immutability();
DROP FUNCTION prevent_row_mutation();
DROP FUNCTION rux_target_os_list_valid(TEXT[]);
DROP FUNCTION rux_semver_identifier_sort_key(TEXT, BOOLEAN);

DROP EXTENSION pg_trgm;
