-- Settings saved from the web UI (ADR 0013). Every save is a new version;
-- history is append-only (INV-16). A NULL value means "file default".
CREATE TABLE settings_versions (
    section    text        NOT NULL,
    version    bigint      NOT NULL,
    value      jsonb,
    updated_by text        NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (section, version)
);
SELECT qd_make_append_only('settings_versions');

-- Encrypted secrets (XChaCha20-Poly1305; the key never touches the database).
-- Deliberately mutable: a cleared or rotated secret must really be gone.
-- Every change is written to audit_log (name only, never the value).
CREATE TABLE secrets (
    name       text        PRIMARY KEY,
    ciphertext bytea       NOT NULL,
    updated_by text        NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
