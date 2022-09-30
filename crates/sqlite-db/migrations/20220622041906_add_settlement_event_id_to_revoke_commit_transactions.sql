ALTER TABLE
    revoked_commit_transactions
ADD
    -- We allow NULL values to ensure backwards compatibility (we cannot chose a default value for this easily)
    COLUMN settlement_event_id text NULL;
