-- Introduce contract_symbol field for all cfd tables.
--
-- Default to BTCUSD for all old already opened CFDs, as this is the only
-- contract symbol active for now.
ALTER TABLE
    cfds
ADD
    COLUMN contract_symbol NOT NULL DEFAULT 'BtcUsd';
ALTER TABLE
    closed_cfds
ADD
    COLUMN contract_symbol NOT NULL DEFAULT 'BtcUsd';
ALTER TABLE
    failed_cfds
ADD
    COLUMN contract_symbol NOT NULL DEFAULT 'BtcUsd';
