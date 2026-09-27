-- Users and sessions for the API (ADR 0008).
-- Passwords are argon2id PHC strings; sessions store only SHA-256(token).

CREATE TABLE users (
    user_id       uuid        PRIMARY KEY,
    username      text        NOT NULL UNIQUE,
    role          text        NOT NULL CHECK (role IN ('owner', 'viewer')),
    password_hash text        NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Exactly one owner (spec §2).
CREATE UNIQUE INDEX users_single_owner ON users (role) WHERE role = 'owner';

CREATE TABLE sessions (
    token_hash       text        PRIMARY KEY,
    user_id          uuid        NOT NULL REFERENCES users (user_id),
    expires_at       timestamptz NOT NULL,
    stepped_up_until timestamptz,
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sessions_user_idx ON sessions (user_id);
