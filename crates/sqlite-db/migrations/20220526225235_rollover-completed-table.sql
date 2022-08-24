-- Add migration script here
CREATE TABLE IF NOT EXISTS rollover_completed_event_data (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer UNIQUE NOT NULL,
    event_id integer NOT NULL,
    settlement_event_id text NOT NULL,
    refund_timelock text NOT NULL,
    funding_fee number NOT NULL,
    rate text NOT NULL,
    identity text NOT NULL,
    identity_counterparty text NOT NULL,
    maker_address text NOT NULL,
    taker_address text NOT NULL,
    maker_lock_amount number NOT NULL,
    taker_lock_amount number NOT NULL,
    publish_sk text NOT NULL,
    publish_pk_counterparty text NOT NULL,
    revocation_secret text NOT NULL,
    revocation_pk_counterparty text NOT NULL,
    lock_tx text NOT NULL,
    lock_tx_descriptor text NOT NULL,
    commit_tx text NOT NULL,
    commit_adaptor_signature text NOT NULL,
    commit_descriptor text NOT NULL,
    refund_tx text NOT NULL,
    refund_signature text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id) ON DELETE CASCADE,
    FOREIGN KEY (event_id) REFERENCES EVENTS (id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS revoked_commit_transactions (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    encsig_ours TEXT NOT NULL,
    publication_pk_theirs TEXT NOT NULL,
    revocation_sk_theirs TEXT NOT NULL,
    script_pubkey TEXT NOT NULL,
    txid TEXT NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS open_cets (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    oracle_event_id TEXT NOT NULL,
    adaptor_sig TEXT NOT NULL,
    maker_amount INTEGER NOT NULL,
    taker_amount INTEGER NOT NULL,
    n_bits TEXT NOT NULL,
    range_end INTEGER NOT NULL,
    range_start INTEGER NOT NULL,
    txid TEXT NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id) ON DELETE CASCADE
);
