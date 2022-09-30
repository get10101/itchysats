-- Add migration script here
ALTER TABLE
    orders
ADD
    COLUMN fee_rate NOT NULL DEFAULT 1;
