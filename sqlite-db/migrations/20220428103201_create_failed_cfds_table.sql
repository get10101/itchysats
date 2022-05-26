CREATE TABLE IF NOT EXISTS failed_cfds (
    id integer PRIMARY KEY autoincrement,
    uuid text UNIQUE NOT NULL,
    position text NOT NULL,
    initial_price text NOT NULL,
    taker_leverage integer NOT NULL,
    n_contracts integer NOT NULL,
    counterparty_network_identity text NOT NULL,
    role text NOT NULL,
    fees integer NOT NULL,
    kind text NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS failed_cfds_uuid ON failed_cfds (uuid);
CREATE TABLE IF NOT EXISTS event_log_failed (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    name text NOT NULL,
    created_at integer NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES failed_cfds (id)
)
