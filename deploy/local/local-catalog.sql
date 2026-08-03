BEGIN;

-- Keep concurrent invocations from interleaving their natural-key lookups.
SELECT pg_advisory_xact_lock(2026080102050);

INSERT INTO namespaces (display_name, created_at, updated_at)
VALUES
    ('Rux', '2026-01-01 00:00:00+00', '2026-01-01 00:00:00+00'),
    ('Community_Tools', '2026-01-02 00:00:00+00', '2026-01-02 00:00:00+00'),
    ('Acme', '2026-01-03 00:00:00+00', '2026-01-03 00:00:00+00')
ON CONFLICT DO NOTHING;

WITH seed_packages (namespace_name, package_name, created_at) AS (
    VALUES
        ('rux', 'Io', '2026-01-01 01:00:00+00'::timestamptz),
        ('rux', 'Json', '2026-01-01 02:00:00+00'::timestamptz),
        ('community-tools', 'Http_Client', '2026-01-02 01:00:00+00'::timestamptz),
        ('acme', 'Registry_Cli', '2026-01-03 01:00:00+00'::timestamptz)
)
INSERT INTO packages (namespace_id, display_name, created_at)
SELECT namespaces.id, seed_packages.package_name, seed_packages.created_at
FROM seed_packages
JOIN namespaces ON namespaces.normalized_name = seed_packages.namespace_name
ON CONFLICT DO NOTHING;

WITH seed_versions (
    namespace_name,
    package_name,
    version,
    major,
    minor,
    patch,
    prerelease,
    build_metadata,
    min_rux,
    package_type,
    description,
    repository_url,
    homepage_url,
    readme_path,
    readme_text,
    license_expression,
    license_file_path,
    license_file_text,
    normalized_manifest,
    artifact_sha256,
    artifact_size,
    storage_key,
    artifact_file_count,
    artifact_expanded_bytes,
    source_file_count,
    source_line_count,
    published_at,
    yanked_at
) AS (
    VALUES
        (
            'rux', 'io', '1.0.0', 1, 0, 0, NULL, NULL, '0.4.0', 'source',
            'Portable input and output primitives for Rux.',
            'https://github.com/rux-lang/io', 'https://rux-lang.dev/packages/io',
            'README.md', '# Rux Io\n\nPortable input and output primitives.',
            'MIT OR Apache-2.0', NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Rux","name":"Io","version":"1.0.0","type":"source","description":"Portable input and output primitives for Rux.","authors":["Rux Contributors"],"keywords":["io","runtime"],"license":"MIT OR Apache-2.0","repository":"https://github.com/rux-lang/io","homepage":"https://rux-lang.dev/packages/io","readme":"README.md"},"dependencies":{}}$json$::jsonb,
            repeat('10', 32), 4096, 'local-seed/rux/io/1.0.0.ruxpkg', 4, 8192, 2, 180,
            '2026-01-10 10:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'rux', 'io', '1.1.0', 1, 1, 0, NULL, NULL, '0.4.0', 'source',
            'Async-ready input and output primitives for Rux.',
            'https://github.com/rux-lang/io', 'https://rux-lang.dev/packages/io',
            'README.md', '# Rux Io\n\nStreams, files, and async-ready adapters.',
            'MIT OR Apache-2.0', NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Rux","name":"Io","version":"1.1.0","type":"source","description":"Async-ready input and output primitives for Rux.","authors":["Rux Contributors"],"keywords":["io","async"],"license":"MIT OR Apache-2.0","repository":"https://github.com/rux-lang/io","homepage":"https://rux-lang.dev/packages/io","readme":"README.md"},"dependencies":{}}$json$::jsonb,
            repeat('11', 32), 4608, 'local-seed/rux/io/1.1.0.ruxpkg', 5, 9216, 3, 225,
            '2026-02-10 10:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'rux', 'json', '1.0.0', 1, 0, 0, NULL, NULL, '0.4.0', 'library',
            'JSON parsing and serialization.',
            'https://github.com/rux-lang/json', NULL,
            'README.md', '# Rux Json\n\nParse and serialize JSON documents.',
            'MIT', NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Rux","name":"Json","version":"1.0.0","type":"library","description":"JSON parsing and serialization.","authors":["Rux Contributors"],"keywords":["json","serialization"],"license":"MIT","repository":"https://github.com/rux-lang/json","readme":"README.md"},"dependencies":{"io":{"namespace":"Rux","package":"Io","version":"^1.0"}}}$json$::jsonb,
            repeat('20', 32), 5120, 'local-seed/rux/json/1.0.0.ruxpkg', 4, 10240, 2, 310,
            '2026-01-15 12:00:00+00'::timestamptz, '2026-03-01 09:00:00+00'::timestamptz
        ),
        (
            'rux', 'json', '1.1.0-beta.1', 1, 1, 0, 'beta.1', NULL, '0.4.0', 'library',
            NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Rux","name":"Json","version":"1.1.0-beta.1","type":"library","authors":["Rux Contributors"],"keywords":["json","preview"]},"dependencies":{"io":{"namespace":"Rux","package":"Io","version":"^1.1"}}}$json$::jsonb,
            repeat('21', 32), 4864, 'local-seed/rux/json/1.1.0-beta.1.ruxpkg', 4, 9728, 2, 335,
            '2026-02-20 12:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'rux', 'json', '1.1.0', 1, 1, 0, NULL, NULL, '0.4.0', 'library',
            'Fast JSON parsing with streaming support.',
            'https://github.com/rux-lang/json', 'https://rux-lang.dev/packages/json',
            'README.md', '# Rux Json\n\nStreaming JSON parsing and serialization.',
            'MIT', NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Rux","name":"Json","version":"1.1.0","type":"library","description":"Fast JSON parsing with streaming support.","authors":["Rux Contributors"],"keywords":["json","serialization","streaming"],"license":"MIT","repository":"https://github.com/rux-lang/json","homepage":"https://rux-lang.dev/packages/json","readme":"README.md"},"dependencies":{"io":{"namespace":"Rux","package":"Io","version":"^1.1"}}}$json$::jsonb,
            repeat('22', 32), 5632, 'local-seed/rux/json/1.1.0.ruxpkg', 5, 11264, 3, 390,
            '2026-03-10 12:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'community-tools', 'http-client', '0.9.0', 0, 9, 0, NULL, NULL, '0.4.0', 'source',
            'A small HTTP client for Rux applications.',
            'https://github.com/rux-community/http-client', NULL,
            'README.md', '# Http Client\n\nMake HTTP requests from Rux.',
            'Apache-2.0', NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Community_Tools","name":"Http_Client","version":"0.9.0","type":"source","description":"A small HTTP client for Rux applications.","authors":["Rux Community"],"keywords":["http","networking"],"license":"Apache-2.0","repository":"https://github.com/rux-community/http-client","readme":"README.md"},"dependencies":{"io":{"namespace":"Rux","package":"Io","version":"^1.0"},"json":{"namespace":"Rux","package":"Json","version":"^1.0"}}}$json$::jsonb,
            repeat('30', 32), 6144, 'local-seed/community-tools/http-client/0.9.0.ruxpkg', 6, 12288, 4, 480,
            '2026-02-01 08:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'community-tools', 'http-client', '1.0.0', 1, 0, 0, NULL, NULL, '0.4.0', 'source',
            'HTTP client with streaming bodies and JSON helpers.',
            'https://github.com/rux-community/http-client', 'https://community.rux-lang.dev/http-client',
            'README.md', '# Http Client\n\nStreaming requests, responses, and JSON helpers.',
            'Apache-2.0', NULL, NULL,
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Community_Tools","name":"Http_Client","version":"1.0.0","type":"source","description":"HTTP client with streaming bodies and JSON helpers.","authors":["Rux Community","Casey Example"],"keywords":["http","networking","web"],"license":"Apache-2.0","repository":"https://github.com/rux-community/http-client","homepage":"https://community.rux-lang.dev/http-client","readme":"README.md"},"dependencies":{"io":{"namespace":"Rux","package":"Io","version":"^1.1"},"json":{"namespace":"Rux","package":"Json","version":"^1.1"}}}$json$::jsonb,
            repeat('31', 32), 7168, 'local-seed/community-tools/http-client/1.0.0.ruxpkg', 7, 14336, 5, 610,
            '2026-04-01 08:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'acme', 'registry-cli', '2.0.0+native', 2, 0, 0, NULL, 'native', '0.4.0', 'program',
            'Command-line tools for exploring a Rux registry.',
            'https://example.com/acme/registry-cli', 'https://example.com/registry-cli',
            'README.md', '# Registry CLI\n\nBrowse and inspect registry packages.',
            NULL, 'LICENSE.md', 'Example fixture license. Local development only.',
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Acme","name":"Registry_Cli","version":"2.0.0+native","type":"program","description":"Command-line tools for exploring a Rux registry.","authors":["Acme Tools Team"],"keywords":["registry","command-line"],"license_file":"LICENSE.md","repository":"https://example.com/acme/registry-cli","homepage":"https://example.com/registry-cli","readme":"README.md"},"dependencies":{"http":{"namespace":"Community_Tools","package":"Http_Client","version":"^1.0"}}}$json$::jsonb,
            repeat('40', 32), 8192, 'local-seed/acme/registry-cli/2.0.0+native.ruxpkg', 8, 16384, 5, 720,
            '2026-05-01 15:00:00+00'::timestamptz, NULL::timestamptz
        ),
        (
            'acme', 'registry-cli', '2.0.0+portable', 2, 0, 0, NULL, 'portable', '0.4.0', 'program',
            'Portable command-line tools for exploring a Rux registry.',
            'https://example.com/acme/registry-cli', 'https://example.com/registry-cli',
            'README.md', '# Registry CLI\n\nPortable registry browsing and inspection.',
            NULL, 'LICENSE.md', 'Example fixture license. Local development only.',
            $json${"manifest":{"version":1,"min_rux":"0.4.0"},"package":{"namespace":"Acme","name":"Registry_Cli","version":"2.0.0+portable","type":"program","description":"Portable command-line tools for exploring a Rux registry.","authors":["Acme Tools Team"],"keywords":["registry","portable"],"license_file":"LICENSE.md","repository":"https://example.com/acme/registry-cli","homepage":"https://example.com/registry-cli","readme":"README.md"},"dependencies":{"http":{"namespace":"Community_Tools","package":"Http_Client","version":"^1.0"}}}$json$::jsonb,
            repeat('41', 32), 7680, 'local-seed/acme/registry-cli/2.0.0+portable.ruxpkg', 7, 15360, 4, 680,
            '2026-05-01 15:05:00+00'::timestamptz, NULL::timestamptz
        )
)
INSERT INTO package_versions (
    package_id,
    version,
    major,
    minor,
    patch,
    prerelease,
    build_metadata,
    manifest_schema_version,
    min_rux,
    package_type,
    description,
    repository_url,
    homepage_url,
    readme_path,
    readme_text,
    license_expression,
    license_file_path,
    license_file_text,
    normalized_manifest,
    artifact_sha256,
    artifact_size,
    storage_key,
    artifact_file_count,
    artifact_expanded_bytes,
    source_file_count,
    source_line_count,
    published_at,
    yanked_at
)
SELECT
    packages.id,
    seed_versions.version,
    seed_versions.major,
    seed_versions.minor,
    seed_versions.patch,
    seed_versions.prerelease,
    seed_versions.build_metadata,
    1,
    seed_versions.min_rux,
    seed_versions.package_type,
    seed_versions.description,
    seed_versions.repository_url,
    seed_versions.homepage_url,
    seed_versions.readme_path,
    seed_versions.readme_text,
    seed_versions.license_expression,
    seed_versions.license_file_path,
    seed_versions.license_file_text,
    seed_versions.normalized_manifest,
    decode(seed_versions.artifact_sha256, 'hex'),
    seed_versions.artifact_size,
    seed_versions.storage_key,
    seed_versions.artifact_file_count,
    seed_versions.artifact_expanded_bytes,
    seed_versions.source_file_count,
    seed_versions.source_line_count,
    seed_versions.published_at,
    seed_versions.yanked_at
FROM seed_versions
JOIN namespaces ON namespaces.normalized_name = seed_versions.namespace_name
JOIN packages
    ON packages.namespace_id = namespaces.id
    AND packages.normalized_name = seed_versions.package_name
ON CONFLICT DO NOTHING;

WITH seed_authors (namespace_name, package_name, version, ordinal, author) AS (
    VALUES
        ('rux', 'io', '1.0.0', 0, 'Rux Contributors'),
        ('rux', 'io', '1.1.0', 0, 'Rux Contributors'),
        ('rux', 'json', '1.0.0', 0, 'Rux Contributors'),
        ('rux', 'json', '1.1.0-beta.1', 0, 'Rux Contributors'),
        ('rux', 'json', '1.1.0', 0, 'Rux Contributors'),
        ('community-tools', 'http-client', '0.9.0', 0, 'Rux Community'),
        ('community-tools', 'http-client', '1.0.0', 0, 'Rux Community'),
        ('community-tools', 'http-client', '1.0.0', 1, 'Casey Example'),
        ('acme', 'registry-cli', '2.0.0+native', 0, 'Acme Tools Team'),
        ('acme', 'registry-cli', '2.0.0+portable', 0, 'Acme Tools Team')
)
INSERT INTO package_version_authors (package_version_id, ordinal, author)
SELECT package_versions.id, seed_authors.ordinal, seed_authors.author
FROM seed_authors
JOIN namespaces ON namespaces.normalized_name = seed_authors.namespace_name
JOIN packages
    ON packages.namespace_id = namespaces.id
    AND packages.normalized_name = seed_authors.package_name
JOIN package_versions
    ON package_versions.package_id = packages.id
    AND package_versions.version = seed_authors.version
ON CONFLICT DO NOTHING;

WITH seed_keywords (namespace_name, package_name, version, ordinal, keyword) AS (
    VALUES
        ('rux', 'io', '1.0.0', 0, 'Io'),
        ('rux', 'io', '1.0.0', 1, 'Runtime'),
        ('rux', 'io', '1.1.0', 0, 'Io'),
        ('rux', 'io', '1.1.0', 1, 'Async'),
        ('rux', 'json', '1.0.0', 0, 'Json'),
        ('rux', 'json', '1.0.0', 1, 'Serialization'),
        ('rux', 'json', '1.1.0-beta.1', 0, 'Json'),
        ('rux', 'json', '1.1.0-beta.1', 1, 'Preview'),
        ('rux', 'json', '1.1.0', 0, 'Json'),
        ('rux', 'json', '1.1.0', 1, 'Serialization'),
        ('rux', 'json', '1.1.0', 2, 'Streaming'),
        ('community-tools', 'http-client', '0.9.0', 0, 'Http'),
        ('community-tools', 'http-client', '0.9.0', 1, 'Networking'),
        ('community-tools', 'http-client', '1.0.0', 0, 'Http'),
        ('community-tools', 'http-client', '1.0.0', 1, 'Networking'),
        ('community-tools', 'http-client', '1.0.0', 2, 'Web'),
        ('acme', 'registry-cli', '2.0.0+native', 0, 'Registry'),
        ('acme', 'registry-cli', '2.0.0+native', 1, 'Command_Line'),
        ('acme', 'registry-cli', '2.0.0+portable', 0, 'Registry'),
        ('acme', 'registry-cli', '2.0.0+portable', 1, 'Portable')
)
INSERT INTO package_version_keywords (package_version_id, ordinal, display_name)
SELECT package_versions.id, seed_keywords.ordinal, seed_keywords.keyword
FROM seed_keywords
JOIN namespaces ON namespaces.normalized_name = seed_keywords.namespace_name
JOIN packages
    ON packages.namespace_id = namespaces.id
    AND packages.normalized_name = seed_keywords.package_name
JOIN package_versions
    ON package_versions.package_id = packages.id
    AND package_versions.version = seed_keywords.version
ON CONFLICT DO NOTHING;

WITH seed_dependencies (
    namespace_name,
    package_name,
    version,
    alias,
    target_namespace,
    target_package,
    version_range
) AS (
    VALUES
        ('rux', 'json', '1.0.0', 'Io', 'Rux', 'Io', '^1.0'),
        ('rux', 'json', '1.1.0-beta.1', 'Io', 'Rux', 'Io', '^1.1'),
        ('rux', 'json', '1.1.0', 'Io', 'Rux', 'Io', '^1.1'),
        ('community-tools', 'http-client', '0.9.0', 'Io', 'Rux', 'Io', '^1.0'),
        ('community-tools', 'http-client', '0.9.0', 'Json', 'Rux', 'Json', '^1.0'),
        ('community-tools', 'http-client', '1.0.0', 'Io', 'Rux', 'Io', '^1.1'),
        ('community-tools', 'http-client', '1.0.0', 'Json', 'Rux', 'Json', '^1.1'),
        ('acme', 'registry-cli', '2.0.0+native', 'Http', 'Community_Tools', 'Http_Client', '^1.0'),
        ('acme', 'registry-cli', '2.0.0+portable', 'Http', 'Community_Tools', 'Http_Client', '^1.0')
)
INSERT INTO dependencies (
    package_version_id,
    display_alias,
    target_namespace_display_name,
    target_package_display_name,
    version_range
)
SELECT
    package_versions.id,
    seed_dependencies.alias,
    seed_dependencies.target_namespace,
    seed_dependencies.target_package,
    seed_dependencies.version_range
FROM seed_dependencies
JOIN namespaces ON namespaces.normalized_name = seed_dependencies.namespace_name
JOIN packages
    ON packages.namespace_id = namespaces.id
    AND packages.normalized_name = seed_dependencies.package_name
JOIN package_versions
    ON package_versions.package_id = packages.id
    AND package_versions.version = seed_dependencies.version
ON CONFLICT DO NOTHING;

COMMIT;
