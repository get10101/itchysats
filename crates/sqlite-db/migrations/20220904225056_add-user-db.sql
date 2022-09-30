-- Add migration script here
CREATE TABLE IF NOT EXISTS login_details (
    id integer PRIMARY KEY autoincrement,
    -- dprint seems to enforce this writing
    PASSWORD text NOT NULL,
    first_login boolean NOT NULL
);
INSERT INTO
    login_details(PASSWORD, first_login)
VALUES
    (
        -- weareallsatoshi
        '$argon2i$v=19$m=4096,t=3,p=1$NjZ9FHVxa21OTA$P0o6y+4HwS51jMhawjEKPUxxi41LyWkIRRZqIEp/4j8',
        TRUE
    );
