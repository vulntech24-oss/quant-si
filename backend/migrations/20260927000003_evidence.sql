-- Recorded evidence (INV-11, ADR 0010): validations and paper reviews.
-- Append-only (INV-16): a verdict is never edited after the fact.
CREATE TABLE evidence_records (
    evidence_id uuid        PRIMARY KEY,
    version_id  uuid        NOT NULL REFERENCES strategy_versions (version_id),
    kind        text        NOT NULL CHECK (kind IN ('validation', 'paper_review')),
    passed      boolean     NOT NULL,
    report      jsonb       NOT NULL,
    created_at  timestamptz NOT NULL
);
CREATE INDEX evidence_records_version_idx ON evidence_records (version_id, created_at);
SELECT qd_make_append_only('evidence_records');
