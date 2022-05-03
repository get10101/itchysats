CREATE TABLE IF NOT EXISTS time_to_first_position (
    id integer PRIMARY KEY autoincrement,
    taker_id text UNIQUE NOT NULL,
    first_seen_timestamp integer NULL,
    first_position_timestamp integer NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS time_to_first_position_taker_id ON time_to_first_position (taker_id);
