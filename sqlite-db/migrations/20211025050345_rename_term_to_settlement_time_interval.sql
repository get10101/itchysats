ALTER TABLE
    orders RENAME COLUMN term_seconds TO settlement_time_interval_seconds;
ALTER TABLE
    orders DROP COLUMN creation_timestamp_nanoseconds;
ALTER TABLE
    orders DROP COLUMN term_nanoseconds;
