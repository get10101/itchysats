-- Add migration script here
ALTER TABLE
    cfds RENAME COLUMN quantity_usd TO contracts;
