ALTER TABLE
    rollover_completed_event_data
ADD
    -- The complete fee including this rollover (contrary to the funding_fee this field contains the accumulated fees until now).
    COLUMN complete_fee INTEGER NULL;
ALTER TABLE
    rollover_completed_event_data
ADD
    COLUMN complete_fee_flow text NULL;
ALTER TABLE
    revoked_commit_transactions
ADD
    -- We add this as metadata to the revoked_commit_transactions so we are able to handle rollovers from a previous commit-tx-id.
    -- This fields allows the maker to know what the complete fee was at a previous point in time.
    COLUMN complete_fee INTEGER NULL;
ALTER TABLE
    revoked_commit_transactions
ADD
    COLUMN complete_fee_flow text NULL;
