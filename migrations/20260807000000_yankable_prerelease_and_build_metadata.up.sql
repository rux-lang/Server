-- `prerelease_sort_key` and `build_metadata_sort_key` are STORED generated
-- columns, and PostgreSQL does not compute them until after BEFORE triggers
-- have run: inside the trigger they read NULL from NEW while OLD carries the
-- stored value. The original immutability check therefore saw a difference on
-- every UPDATE of a version that has a prerelease or build metadata, which made
-- those versions impossible to yank or un-yank.
--
-- Both keys are derived from `prerelease` and `build_metadata`, which are still
-- compared, so excluding them costs no protection.

CREATE OR REPLACE FUNCTION enforce_package_version_immutability()
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
