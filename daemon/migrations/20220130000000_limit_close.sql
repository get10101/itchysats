CREATE TABLE IF NOT EXISTS limit_close (
    id integer PRIMARY KEY autoincrement,
    price text NOT NULL,
    cfd_id text UNIQUE NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id)
);
