ALTER TABLE
    cfds
ADD
    COLUMN initial_tx_fee_rate text NOT NULL DEFAULT '1';
