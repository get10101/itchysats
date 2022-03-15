-- Add migration script here
ALTER TABLE
    orders
ADD
    COLUMN fee_rate integer NOT NULL DEFAULT 1;
