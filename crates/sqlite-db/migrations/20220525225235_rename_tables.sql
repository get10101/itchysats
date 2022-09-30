-- Add migration script here
ALTER TABLE
    cets RENAME TO closed_cets;
ALTER TABLE
    refund_txs RENAME TO closed_refund_txs;
ALTER TABLE
    commit_txs RENAME TO closed_commit_txs;
