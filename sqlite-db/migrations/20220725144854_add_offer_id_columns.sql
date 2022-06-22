-- Introduce offer_id field for all cfd tables.
-- We can opt to move offer related parameters to a separate table for normalization in a separate iteration if needed.
--
-- Steps:
-- 1. Rename `uuid` to `order_id` so the column makes more sense
-- 2. Add column `offer_id` with a dummy default value
-- 3. Set `offer_id` value to `order_id` value because prior to this change offer=order
ALTER TABLE
    cfds RENAME COLUMN uuid TO order_id;
ALTER TABLE
    cfds
ADD
    COLUMN offer_id text NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
UPDATE
    cfds
SET
    offer_id = order_id;
ALTER TABLE
    main.closed_cfds RENAME COLUMN uuid TO order_id;
ALTER TABLE
    closed_cfds
ADD
    COLUMN offer_id text NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
UPDATE
    closed_cfds
SET
    offer_id = order_id;
ALTER TABLE
    main.failed_cfds RENAME COLUMN uuid TO order_id;
ALTER TABLE
    failed_cfds
ADD
    COLUMN offer_id text NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
UPDATE
    failed_cfds
SET
    offer_id = order_id;
