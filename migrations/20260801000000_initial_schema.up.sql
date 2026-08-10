CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE FUNCTION rux_semver_identifier_sort_key(value TEXT, allow_leading_zeros BOOLEAN)
RETURNS BYTEA
LANGUAGE plpgsql
IMMUTABLE
STRICT
PARALLEL SAFE
AS $$
DECLARE
    segment TEXT;
    significant TEXT;
    result BYTEA := ''::bytea;
BEGIN
    FOREACH segment IN ARRAY string_to_array(value, '.') LOOP
        IF segment ~ '^[0-9]+$' THEN
            significant := CASE
                WHEN allow_leading_zeros THEN ltrim(segment, '0')
                ELSE segment
            END;
            result := result
                || decode('01', 'hex')
                || int2send(octet_length(significant)::SMALLINT)
                || convert_to(significant, 'UTF8')
                || int2send(octet_length(segment)::SMALLINT)
                || decode('00', 'hex');
        ELSE
            result := result
                || decode('02', 'hex')
                || convert_to(segment, 'UTF8')
                || decode('00', 'hex');
        END IF;
    END LOOP;
    RETURN result;
END;
$$;

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    github_user_id NUMERIC(20, 0) UNIQUE,
    github_login VARCHAR(39),
    normalized_github_login VARCHAR(39)
        GENERATED ALWAYS AS (lower(github_login)) STORED,
    display_name TEXT,
    avatar_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    anonymized_at TIMESTAMPTZ,
    CONSTRAINT users_github_identity_state CHECK (
        (anonymized_at IS NULL AND github_user_id IS NOT NULL AND github_login IS NOT NULL)
        OR
        (anonymized_at IS NOT NULL AND github_user_id IS NULL AND github_login IS NULL)
    ),
    CONSTRAINT users_github_user_id_range CHECK (
        github_user_id IS NULL
        OR github_user_id BETWEEN 1 AND 18446744073709551615
    ),
    CONSTRAINT users_github_login_syntax CHECK (
        github_login IS NULL
        OR (
            octet_length(github_login) BETWEEN 1 AND 39
            AND github_login ~ '^[A-Za-z0-9]+(-[A-Za-z0-9]+)*$'
        )
    ),
    CONSTRAINT users_display_name_length CHECK (
        display_name IS NULL OR octet_length(display_name) <= 256
    ),
    CONSTRAINT users_avatar_url_length CHECK (
        avatar_url IS NULL OR octet_length(avatar_url) <= 2048
    ),
    CONSTRAINT users_updated_at_order CHECK (updated_at >= created_at),
    CONSTRAINT users_anonymized_at_order CHECK (
        anonymized_at IS NULL OR anonymized_at >= created_at
    ),
    CONSTRAINT users_normalized_github_login_unique UNIQUE (normalized_github_login)
);

CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    secret_hash BYTEA NOT NULL UNIQUE,
    csrf_hash BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    CONSTRAINT sessions_secret_hash_length CHECK (octet_length(secret_hash) = 32),
    CONSTRAINT sessions_csrf_hash_length CHECK (octet_length(csrf_hash) = 32),
    CONSTRAINT sessions_expiry_order CHECK (expires_at > created_at),
    CONSTRAINT sessions_last_seen_order CHECK (last_seen_at >= created_at),
    CONSTRAINT sessions_revoked_at_order CHECK (
        revoked_at IS NULL OR revoked_at >= created_at
    )
);

CREATE TABLE namespaces (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    display_name VARCHAR(64) NOT NULL,
    normalized_name VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(display_name, '_', '-'))) STORED,
    created_by_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT namespaces_display_name_syntax CHECK (
        octet_length(display_name) BETWEEN 1 AND 64
        AND display_name ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT namespaces_updated_at_order CHECK (updated_at >= created_at),
    CONSTRAINT namespaces_normalized_name_unique UNIQUE (normalized_name)
);

CREATE TABLE namespace_owners (
    namespace_id UUID NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    added_by_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (namespace_id, user_id),
    CONSTRAINT namespace_owners_role CHECK (role IN ('owner', 'maintainer'))
);

CREATE TABLE namespace_invitations (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    namespace_id UUID NOT NULL REFERENCES namespaces (id) ON DELETE CASCADE,
    invited_user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    invited_by_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    role TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    CONSTRAINT namespace_invitations_role CHECK (role IN ('owner', 'maintainer')),
    CONSTRAINT namespace_invitations_expiry_order CHECK (expires_at > created_at),
    CONSTRAINT namespace_invitations_resolution CHECK (
        NOT (accepted_at IS NOT NULL AND revoked_at IS NOT NULL)
    ),
    CONSTRAINT namespace_invitations_accepted_at_order CHECK (
        accepted_at IS NULL OR accepted_at BETWEEN created_at AND expires_at
    ),
    CONSTRAINT namespace_invitations_revoked_at_order CHECK (
        revoked_at IS NULL OR revoked_at >= created_at
    ),
    CONSTRAINT namespace_invitations_not_self_invited CHECK (
        invited_by_user_id IS NULL OR invited_by_user_id <> invited_user_id
    )
);

CREATE TABLE packages (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    namespace_id UUID NOT NULL REFERENCES namespaces (id) ON DELETE RESTRICT,
    display_name VARCHAR(64) NOT NULL,
    normalized_name VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(display_name, '_', '-'))) STORED,
    created_by_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT packages_display_name_syntax CHECK (
        octet_length(display_name) BETWEEN 1 AND 64
        AND display_name ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT packages_namespace_normalized_name_unique
        UNIQUE (namespace_id, normalized_name)
);

CREATE TABLE package_versions (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    package_id UUID NOT NULL REFERENCES packages (id) ON DELETE RESTRICT,
    version VARCHAR(256) NOT NULL,
    major NUMERIC(20, 0) NOT NULL,
    minor NUMERIC(20, 0) NOT NULL,
    patch NUMERIC(20, 0) NOT NULL,
    prerelease TEXT,
    build_metadata TEXT,
    manifest_schema_version SMALLINT NOT NULL,
    min_rux VARCHAR(256) NOT NULL,
    package_type TEXT NOT NULL,
    description TEXT,
    repository_url TEXT,
    homepage_url TEXT,
    readme_file_path TEXT,
    readme_file_text TEXT,
    license_expression TEXT,
    license_file_path TEXT,
    license_file_text TEXT,
    normalized_manifest JSONB NOT NULL,
    artifact_sha256 BYTEA NOT NULL,
    artifact_size BIGINT NOT NULL,
    storage_key TEXT NOT NULL UNIQUE,
    artifact_file_count INTEGER NOT NULL,
    artifact_expanded_bytes BIGINT NOT NULL,
    source_file_count INTEGER NOT NULL,
    source_line_count BIGINT NOT NULL,
    published_by_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    yanked_at TIMESTAMPTZ,
    yanked_by_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    search_vector TSVECTOR
        GENERATED ALWAYS AS (
            setweight(to_tsvector('simple', coalesce(description, '')), 'A')
            || setweight(to_tsvector('simple', coalesce(readme_file_text, '')), 'B')
        ) STORED,
    prerelease_sort_key BYTEA
        GENERATED ALWAYS AS (
            rux_semver_identifier_sort_key(prerelease, FALSE)
        ) STORED,
    build_metadata_sort_key BYTEA
        GENERATED ALWAYS AS (
            rux_semver_identifier_sort_key(build_metadata, TRUE)
        ) STORED,
    CONSTRAINT package_versions_package_version_unique UNIQUE (package_id, version),
    CONSTRAINT package_versions_version_length CHECK (octet_length(version) BETWEEN 1 AND 256),
    CONSTRAINT package_versions_component_range CHECK (
        major BETWEEN 0 AND 18446744073709551615
        AND minor BETWEEN 0 AND 18446744073709551615
        AND patch BETWEEN 0 AND 18446744073709551615
    ),
    CONSTRAINT package_versions_prerelease_length CHECK (
        prerelease IS NULL OR octet_length(prerelease) BETWEEN 1 AND 256
    ),
    CONSTRAINT package_versions_build_metadata_length CHECK (
        build_metadata IS NULL OR octet_length(build_metadata) BETWEEN 1 AND 256
    ),
    CONSTRAINT package_versions_manifest_schema CHECK (manifest_schema_version = 1),
    CONSTRAINT package_versions_min_rux_length CHECK (octet_length(min_rux) BETWEEN 1 AND 256),
    CONSTRAINT package_versions_package_type CHECK (
        package_type IN ('executable', 'shared_library', 'static_library', 'source_library')
    ),
    CONSTRAINT package_versions_description_length CHECK (
        description IS NULL OR octet_length(description) <= 2048
    ),
    CONSTRAINT package_versions_repository_url_length CHECK (
        repository_url IS NULL OR octet_length(repository_url) <= 2048
    ),
    CONSTRAINT package_versions_homepage_url_length CHECK (
        homepage_url IS NULL OR octet_length(homepage_url) <= 2048
    ),
    CONSTRAINT package_versions_readme_file_state CHECK (
        (readme_file_path IS NULL AND readme_file_text IS NULL)
        OR (
            readme_file_path IS NOT NULL
            AND readme_file_text IS NOT NULL
            AND octet_length(readme_file_path) BETWEEN 1 AND 2048
            AND octet_length(readme_file_text) <= 1048576
        )
    ),
    CONSTRAINT package_versions_license_state CHECK (
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
    ),
    CONSTRAINT package_versions_normalized_manifest_object CHECK (
        jsonb_typeof(normalized_manifest) = 'object'
    ),
    CONSTRAINT package_versions_artifact_sha256_length CHECK (
        octet_length(artifact_sha256) = 32
    ),
    CONSTRAINT package_versions_artifact_size CHECK (
        artifact_size BETWEEN 1 AND 5242880
    ),
    CONSTRAINT package_versions_storage_key_length CHECK (
        octet_length(storage_key) BETWEEN 1 AND 2048
    ),
    CONSTRAINT package_versions_artifact_file_count CHECK (
        artifact_file_count BETWEEN 2 AND 1024
    ),
    CONSTRAINT package_versions_artifact_expanded_bytes CHECK (
        artifact_expanded_bytes BETWEEN 1 AND 10485760
    ),
    CONSTRAINT package_versions_source_file_count CHECK (
        source_file_count BETWEEN 1 AND artifact_file_count
    ),
    CONSTRAINT package_versions_source_line_count CHECK (source_line_count >= 0),
    CONSTRAINT package_versions_yanked_at_order CHECK (
        yanked_at IS NULL OR yanked_at >= published_at
    ),
    CONSTRAINT package_versions_yanked_by_state CHECK (
        yanked_by_user_id IS NULL OR yanked_at IS NOT NULL
    )
);

CREATE TABLE package_version_authors (
    package_version_id UUID NOT NULL REFERENCES package_versions (id) ON DELETE CASCADE,
    ordinal SMALLINT NOT NULL,
    author TEXT NOT NULL,
    PRIMARY KEY (package_version_id, ordinal),
    CONSTRAINT package_version_authors_ordinal CHECK (ordinal BETWEEN 0 AND 31),
    CONSTRAINT package_version_authors_author_length CHECK (
        octet_length(author) BETWEEN 1 AND 256
    )
);

CREATE TABLE package_version_keywords (
    package_version_id UUID NOT NULL REFERENCES package_versions (id) ON DELETE CASCADE,
    ordinal SMALLINT NOT NULL,
    display_name VARCHAR(64) NOT NULL,
    normalized_name VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(display_name, '_', '-'))) STORED,
    PRIMARY KEY (package_version_id, ordinal),
    CONSTRAINT package_version_keywords_ordinal CHECK (ordinal BETWEEN 0 AND 31),
    CONSTRAINT package_version_keywords_display_name_syntax CHECK (
        octet_length(display_name) BETWEEN 1 AND 64
        AND display_name ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT package_version_keywords_normalized_name_unique
        UNIQUE (package_version_id, normalized_name)
);

CREATE TABLE dependencies (
    package_version_id UUID NOT NULL REFERENCES package_versions (id) ON DELETE CASCADE,
    display_alias VARCHAR(64) NOT NULL,
    normalized_alias VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(display_alias, '_', '-'))) STORED,
    target_namespace_display_name VARCHAR(64) NOT NULL,
    target_namespace_normalized_name VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(target_namespace_display_name, '_', '-'))) STORED,
    target_package_display_name VARCHAR(64) NOT NULL,
    target_package_normalized_name VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(target_package_display_name, '_', '-'))) STORED,
    version_range TEXT NOT NULL,
    PRIMARY KEY (package_version_id, normalized_alias),
    CONSTRAINT dependencies_alias_syntax CHECK (
        octet_length(display_alias) BETWEEN 1 AND 64
        AND display_alias ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT dependencies_target_namespace_syntax CHECK (
        octet_length(target_namespace_display_name) BETWEEN 1 AND 64
        AND target_namespace_display_name ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT dependencies_target_package_syntax CHECK (
        octet_length(target_package_display_name) BETWEEN 1 AND 64
        AND target_package_display_name ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT dependencies_version_range_length CHECK (
        octet_length(version_range) BETWEEN 1 AND 512
    )
);

CREATE TABLE api_tokens (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    display_name TEXT NOT NULL,
    token_prefix TEXT NOT NULL UNIQUE,
    secret_hash BYTEA NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    CONSTRAINT api_tokens_display_name_length CHECK (
        octet_length(display_name) BETWEEN 1 AND 100
    ),
    CONSTRAINT api_tokens_prefix_syntax CHECK (
        octet_length(token_prefix) BETWEEN 8 AND 32
        AND token_prefix ~ '^[A-Za-z0-9_-]+$'
    ),
    CONSTRAINT api_tokens_secret_hash_length CHECK (octet_length(secret_hash) = 32),
    CONSTRAINT api_tokens_last_used_at_order CHECK (
        last_used_at IS NULL OR last_used_at >= created_at
    ),
    CONSTRAINT api_tokens_expires_at_order CHECK (
        expires_at IS NULL OR expires_at > created_at
    ),
    CONSTRAINT api_tokens_revoked_at_order CHECK (
        revoked_at IS NULL OR revoked_at >= created_at
    )
);

CREATE TABLE api_token_scopes (
    api_token_id UUID NOT NULL REFERENCES api_tokens (id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    PRIMARY KEY (api_token_id, scope),
    CONSTRAINT api_token_scopes_scope CHECK (scope IN ('publish', 'yank', 'namespace'))
);

CREATE TABLE audit_records (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    actor_user_id UUID REFERENCES users (id) ON DELETE RESTRICT,
    actor_token_id UUID REFERENCES api_tokens (id) ON DELETE RESTRICT,
    action TEXT NOT NULL,
    subject_type TEXT NOT NULL,
    subject_key TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    namespace_key TEXT
        GENERATED ALWAYS AS (
            CASE
                WHEN subject_type = 'namespace' THEN subject_key
                WHEN subject_type = 'package_version'
                    THEN lower(replace(metadata ->> 'namespace', '_', '-'))
                ELSE NULL
            END
        ) STORED,
    CONSTRAINT audit_records_action_syntax CHECK (
        octet_length(action) BETWEEN 1 AND 100
        AND action ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT audit_records_subject_type_syntax CHECK (
        octet_length(subject_type) BETWEEN 1 AND 100
        AND subject_type ~ '^[a-z][a-z0-9_]*$'
    ),
    CONSTRAINT audit_records_subject_key_length CHECK (
        subject_key IS NULL OR octet_length(subject_key) BETWEEN 1 AND 256
    ),
    CONSTRAINT audit_records_metadata_object CHECK (jsonb_typeof(metadata) = 'object'),
    CONSTRAINT audit_records_token_actor_state CHECK (
        actor_token_id IS NULL OR actor_user_id IS NOT NULL
    )
);

CREATE TABLE download_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    package_version_id UUID NOT NULL REFERENCES package_versions (id) ON DELETE RESTRICT,
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE blocked_identities (
    identity_kind TEXT NOT NULL,
    display_name VARCHAR(64) NOT NULL,
    normalized_name VARCHAR(64)
        GENERATED ALWAYS AS (lower(replace(display_name, '_', '-'))) STORED,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT blocked_identities_kind CHECK (identity_kind IN ('namespace', 'package')),
    CONSTRAINT blocked_identities_display_name_syntax CHECK (
        octet_length(display_name) BETWEEN 1 AND 64
        AND display_name ~ '^[A-Za-z0-9]+([-_][A-Za-z0-9]+)*$'
    ),
    CONSTRAINT blocked_identities_kind_normalized_name_unique
        UNIQUE (identity_kind, normalized_name)
);

CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_active_expiry_idx ON sessions (expires_at) WHERE revoked_at IS NULL;

CREATE INDEX namespaces_created_by_user_id_idx ON namespaces (created_by_user_id);
CREATE INDEX namespaces_normalized_name_trgm_idx
    ON namespaces USING GIN (normalized_name gin_trgm_ops);

CREATE INDEX namespace_owners_user_id_idx ON namespace_owners (user_id);
CREATE INDEX namespace_owners_added_by_user_id_idx ON namespace_owners (added_by_user_id);

CREATE UNIQUE INDEX namespace_invitations_one_pending_idx
    ON namespace_invitations (namespace_id, invited_user_id)
    WHERE accepted_at IS NULL AND revoked_at IS NULL;
CREATE INDEX namespace_invitations_invited_user_id_idx
    ON namespace_invitations (invited_user_id);
CREATE INDEX namespace_invitations_invited_by_user_id_idx
    ON namespace_invitations (invited_by_user_id);

CREATE INDEX packages_created_by_user_id_idx ON packages (created_by_user_id);
CREATE INDEX packages_normalized_name_idx
    ON packages (normalized_name, namespace_id);
CREATE INDEX packages_normalized_name_trgm_idx
    ON packages USING GIN (normalized_name gin_trgm_ops);

CREATE INDEX package_versions_published_by_user_id_idx
    ON package_versions (published_by_user_id);
CREATE INDEX package_versions_yanked_by_user_id_idx
    ON package_versions (yanked_by_user_id);
CREATE INDEX package_versions_search_vector_idx
    ON package_versions USING GIN (search_vector);

CREATE INDEX package_versions_representative_idx
ON package_versions (
    package_id,
    ((yanked_at IS NULL)) DESC,
    ((prerelease IS NULL)) DESC,
    major DESC,
    minor DESC,
    patch DESC,
    prerelease_sort_key DESC,
    ((build_metadata IS NOT NULL)) DESC,
    build_metadata_sort_key DESC,
    id DESC
);

CREATE INDEX package_versions_history_idx
ON package_versions (
    package_id,
    major DESC,
    minor DESC,
    patch DESC,
    ((prerelease IS NULL)) DESC,
    prerelease_sort_key DESC,
    ((build_metadata IS NOT NULL)) DESC,
    build_metadata_sort_key DESC
);

CREATE INDEX package_versions_recent_publication_idx
ON package_versions (package_id, published_at DESC, id DESC);

CREATE INDEX package_version_keywords_normalized_name_idx
    ON package_version_keywords (normalized_name, package_version_id);
CREATE INDEX package_version_keywords_normalized_name_trgm_idx
    ON package_version_keywords USING GIN (normalized_name gin_trgm_ops);

CREATE INDEX dependencies_target_identity_idx
    ON dependencies (
        target_namespace_normalized_name,
        target_package_normalized_name,
        package_version_id
    );

CREATE INDEX api_tokens_user_id_idx ON api_tokens (user_id);
CREATE INDEX api_tokens_active_expiry_idx ON api_tokens (expires_at) WHERE revoked_at IS NULL;

CREATE INDEX audit_records_actor_user_id_idx ON audit_records (actor_user_id);
CREATE INDEX audit_records_actor_token_id_idx ON audit_records (actor_token_id);
CREATE INDEX audit_records_occurred_at_idx ON audit_records (occurred_at, id);
CREATE INDEX audit_records_namespace_activity_idx
ON audit_records (namespace_key, occurred_at DESC, id DESC)
WHERE namespace_key IS NOT NULL;

CREATE INDEX download_events_package_version_occurred_at_idx
    ON download_events (package_version_id, occurred_at, id);
CREATE INDEX download_events_occurred_at_package_version_idx
ON download_events (occurred_at DESC, package_version_id);

CREATE FUNCTION prevent_row_mutation()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION '% records are append-only', TG_TABLE_NAME
        USING ERRCODE = '55000';
END;
$$;

CREATE FUNCTION enforce_package_version_immutability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF (
        to_jsonb(NEW)
        - 'yanked_at'
        - 'yanked_by_user_id'
        - 'search_vector'
        - 'prerelease_sort_key'
        - 'build_metadata_sort_key'
    ) IS DISTINCT FROM (
        to_jsonb(OLD)
        - 'yanked_at'
        - 'yanked_by_user_id'
        - 'search_vector'
        - 'prerelease_sort_key'
        - 'build_metadata_sort_key'
    ) THEN
        RAISE EXCEPTION 'package version metadata is immutable'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER package_versions_immutable_metadata
BEFORE UPDATE ON package_versions
FOR EACH ROW EXECUTE FUNCTION enforce_package_version_immutability();

CREATE TRIGGER package_version_authors_append_only
BEFORE UPDATE OR DELETE ON package_version_authors
FOR EACH ROW EXECUTE FUNCTION prevent_row_mutation();

CREATE TRIGGER package_version_keywords_append_only
BEFORE UPDATE OR DELETE ON package_version_keywords
FOR EACH ROW EXECUTE FUNCTION prevent_row_mutation();

CREATE TRIGGER dependencies_append_only
BEFORE UPDATE OR DELETE ON dependencies
FOR EACH ROW EXECUTE FUNCTION prevent_row_mutation();

CREATE TRIGGER audit_records_append_only
BEFORE UPDATE OR DELETE ON audit_records
FOR EACH ROW EXECUTE FUNCTION prevent_row_mutation();

CREATE TRIGGER download_events_append_only
BEFORE UPDATE OR DELETE ON download_events
FOR EACH ROW EXECUTE FUNCTION prevent_row_mutation();
