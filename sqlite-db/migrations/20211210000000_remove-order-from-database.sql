DROP TABLE cfd_states;
DROP TABLE cfds;
DROP TABLE orders;
CREATE TABLE IF NOT EXISTS cfds (
    id integer PRIMARY KEY autoincrement,
    uuid text UNIQUE NOT NULL,
    trading_pair text NOT NULL,
    position text NOT NULL,
    initial_price text NOT NULL,
    leverage integer NOT NULL,
    liquidation_price text NOT NULL,
    creation_timestamp_seconds integer NOT NULL,
    settlement_time_interval_seconds integer NOT NULL,
    origin text NOT NULL,
    oracle_event_id text NOT NULL,
    fee_rate integer NOT NULL,
    quantity_usd text NOT NULL,
    counterparty text NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS cfd_uuid ON cfds (uuid);
CREATE TABLE IF NOT EXISTS cfd_states (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    state text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id)
);
