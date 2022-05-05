-- The default value is a dummy value that indicates that this is an "old" CFD
-- Eventually, when there is only the libp2p connection available and all old CFDs have been closed all dummy values will have been removed.
ALTER TABLE
    cfds
ADD
    COLUMN counterparty_peer_id TEXT NOT NULL DEFAULT '12D3KooWE1krEN7utXUFhLmLXLWPSuah5WQ7ChzUFetkMS7TRXsm';
