CREATE TABLE revoked_commit_transactions_temp (
    id integer PRIMARY KEY autoincrement,
    cfd_id integer NOT NULL,
    encsig_ours TEXT NOT NULL,
    publication_pk_theirs TEXT NOT NULL,
    revocation_sk_theirs TEXT NULL,
    script_pubkey TEXT NOT NULL,
    txid TEXT NOT NULL,
    revocation_sk_ours text NULL,
    complete_fee INTEGER NULL,
    complete_fee_flow text NULL,
    settlement_event_id text NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id) ON DELETE CASCADE
);
INSERT INTO
    revoked_commit_transactions_temp (
        id,
        cfd_id,
        encsig_ours,
        publication_pk_theirs,
        revocation_sk_theirs,
        script_pubkey,
        txid,
        revocation_sk_ours,
        complete_fee,
        complete_fee_flow,
        settlement_event_id
    )
SELECT
    id,
    cfd_id,
    encsig_ours,
    publication_pk_theirs,
    revocation_sk_theirs,
    script_pubkey,
    txid,
    revocation_sk_ours,
    complete_fee,
    complete_fee_flow,
    settlement_event_id
FROM
    revoked_commit_transactions;
DROP TABLE revoked_commit_transactions;
ALTER TABLE
    revoked_commit_transactions_temp RENAME TO revoked_commit_transactions;
