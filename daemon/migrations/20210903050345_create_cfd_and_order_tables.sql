-- todo: Decimal is had to deserialize as number so we use text
CREATE TABLE IF NOT EXISTS orders (
    id integer PRIMARY KEY autoincrement,
    uuid text UNIQUE NOT NULL,
    trading_pair text NOT NULL,
    position text NOT NULL,
    initial_price text NOT NULL,
    min_quantity text NOT NULL,
    max_quantity text NOT NULL,
    leverage integer NOT NULL,
    liquidation_price text NOT NULL,
    creation_timestamp_seconds integer NOT NULL,
    creation_timestamp_nanoseconds integer NOT NULL,
    term_seconds integer NOT NULL,
    term_nanoseconds integer NOT NULL,
    origin text NOT NULL,
    oracle_event_id text NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS orders_uuid ON orders (uuid);
CREATE TABLE IF NOT EXISTS cfds (
    id integer PRIMARY KEY autoincrement,
    order_id integer UNIQUE NOT NULL,
    order_uuid text UNIQUE NOT NULL,
    quantity_usd text NOT NULL,
    FOREIGN KEY (order_id) REFERENCES orders (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS cfd_order_uuid ON cfds (order_uuid);
CREATE TABLE IF NOT EXISTS cfd_states (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    state text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id)
);
