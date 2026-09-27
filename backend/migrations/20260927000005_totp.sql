-- TOTP second factor (ADR 0015). The secret is encrypted with the master
-- key (XChaCha20-Poly1305, associated data "totp:<user_id>"), like API keys.
-- last_step stops a code from being used twice.
CREATE TABLE user_totp (
    user_id    uuid        PRIMARY KEY REFERENCES users (user_id),
    ciphertext bytea       NOT NULL,
    enabled    boolean     NOT NULL DEFAULT false,
    last_step  bigint      NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
