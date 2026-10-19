ALTER TABLE metric.debug_files
    DROP CONSTRAINT debug_files_file_type_check,
    ADD CONSTRAINT debug_files_file_type_check CHECK(file_type BETWEEN 0 AND 6);
