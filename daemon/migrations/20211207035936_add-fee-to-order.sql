-- Add migration script here
ALTER TABLE orders ADD COLUMN fee_rate not null default 1;
