DROP TABLE cfd_states;
DROP TABLE cfds;
CREATE TABLE IF NOT EXISTS cfds (
    id integer PRIMARY KEY autoincrement,
    uuid text UNIQUE NOT NULL,
    position text NOT NULL,
    initial_price text NOT NULL,
    leverage integer NOT NULL,
    settlement_time_interval_hours integer NOT NULL,
    quantity_usd text NOT NULL,
    counterparty_network_identity text NOT NULL,
    role text NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS cfds_uuid ON cfds (uuid);
CREATE TABLE IF NOT EXISTS EVENTS (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    name text NOT NULL,
    data text NOT NULL,
    created_at text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id)
)
