CREATE TABLE IF NOT EXISTS closed_cfds (
    id integer PRIMARY KEY autoincrement,
    uuid text UNIQUE NOT NULL,
    position text NOT NULL,
    initial_price text NOT NULL,
    taker_leverage integer NOT NULL,
    n_contracts integer NOT NULL,
    counterparty_network_identity text NOT NULL,
    role text NOT NULL,
    fees integer NOT NULL,
    expiry_timestamp integer NOT NULL,
    lock_txid text NOT NULL,
    lock_dlc_vout integer NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS closed_cfds_uuid ON closed_cfds (uuid);
CREATE TABLE IF NOT EXISTS collaborative_settlement_txs (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    txid text NOT NULL,
    vout integer NOT NULL,
    payout integer NOT NULL,
    price text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES closed_cfds (id)
);
CREATE TABLE IF NOT EXISTS commit_txs (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    txid text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES closed_cfds (id)
);
CREATE TABLE IF NOT EXISTS cets (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    txid text NOT NULL,
    vout integer NOT NULL,
    payout integer NOT NULL,
    price text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES closed_cfds (id)
);
CREATE TABLE IF NOT EXISTS refund_txs (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    txid text NOT NULL,
    vout integer NOT NULL,
    payout integer NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES closed_cfds (id)
);
CREATE TABLE IF NOT EXISTS event_log (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    name text NOT NULL,
    created_at integer NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES closed_cfds (id)
)
