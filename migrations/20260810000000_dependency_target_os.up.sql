CREATE FUNCTION rux_target_os_list_valid(TEXT[])
RETURNS BOOLEAN
LANGUAGE SQL
IMMUTABLE
STRICT
PARALLEL SAFE
RETURN cardinality($1) <= 8
    AND $1 <@ ARRAY[
        'Windows',
        'Linux',
        'MacOS',
        'FreeBSD',
        'OpenBSD',
        'NetBSD',
        'DragonFlyBSD',
        'Illumos'
    ]::TEXT[]
    AND cardinality($1) = (
        SELECT count(DISTINCT target)
        FROM unnest($1) AS target
    );

ALTER TABLE dependencies
    ADD COLUMN target_os TEXT[] NOT NULL DEFAULT ARRAY[]::TEXT[],
    ADD CONSTRAINT dependencies_target_os_valid CHECK (rux_target_os_list_valid(target_os));
