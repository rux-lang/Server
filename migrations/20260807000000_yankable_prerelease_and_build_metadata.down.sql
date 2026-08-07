CREATE OR REPLACE FUNCTION enforce_package_version_immutability()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $$
BEGIN
    IF (to_jsonb(NEW) - 'yanked_at' - 'yanked_by_user_id' - 'search_vector')
        IS DISTINCT FROM
       (to_jsonb(OLD) - 'yanked_at' - 'yanked_by_user_id' - 'search_vector') THEN
        RAISE EXCEPTION 'package version metadata is immutable'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
$$;
