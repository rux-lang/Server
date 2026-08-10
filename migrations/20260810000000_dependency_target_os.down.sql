ALTER TABLE dependencies
    DROP COLUMN target_os;

DROP FUNCTION rux_target_os_list_valid(TEXT[]);
