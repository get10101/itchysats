-- The default value is a placeholder value that indicates that this is an "old" CFD
-- The placeholder value is deterministically derived from the identities of a seed filled with zeros.
-- Eventually, when there is only the libp2p connection available and all old CFDs have been closed all placeholder values will have been removed.
ALTER TABLE
    cfds
ADD
    COLUMN counterparty_peer_id TEXT NOT NULL DEFAULT '12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm';
ALTER TABLE
    closed_cfds
ADD
    COLUMN counterparty_peer_id TEXT NOT NULL DEFAULT '12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm';
ALTER TABLE
    failed_cfds
ADD
    COLUMN counterparty_peer_id TEXT NOT NULL DEFAULT '12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm';
