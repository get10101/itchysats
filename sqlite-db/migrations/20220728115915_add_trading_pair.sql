-- Introduce trading_pair field for all cfd tables.
--
-- Default to BTCUSD for all old already opened CFDs, as this is the only
-- trading pair active for now.
ALTER TABLE
    cfds
ADD
    COLUMN trading_pair NOT NULL DEFAULT 'BtcUsd';
ALTER TABLE
    closed_cfds
ADD
    COLUMN trading_pair NOT NULL DEFAULT 'BtcUsd';
ALTER TABLE
    failed_cfds
ADD
    COLUMN trading_pair NOT NULL DEFAULT 'BtcUsd';
