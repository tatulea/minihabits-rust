-- Existing v1 rows deliberately remain unowned. Registration never claims them.
-- Only the local operator's explicit import_legacy operation assigns them.
CREATE TABLE users (
    id TEXT PRIMARY KEY NOT NULL CHECK (length(id) = 36),
    username TEXT UNIQUE NOT NULL
        CHECK (length(username) BETWEEN 3 AND 32
            AND username = lower(username) AND username NOT GLOB '*[^a-z0-9_]*'),
    password_hash TEXT NOT NULL CHECK (password_hash LIKE '$argon2id$%'),
    created_at INTEGER NOT NULL
);
CREATE TABLE sessions (
    token_hash TEXT PRIMARY KEY NOT NULL
        CHECK (length(token_hash) = 64 AND token_hash NOT GLOB '*[^0-9a-f]*'),
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > created_at)
);
CREATE INDEX sessions_by_user ON sessions(user_id);
CREATE INDEX sessions_by_expiry ON sessions(expires_at);
ALTER TABLE systems ADD COLUMN user_id TEXT REFERENCES users(id) ON DELETE RESTRICT;
CREATE INDEX systems_by_user ON systems(user_id, created_at, id);
-- Retain the complete original table for explicit import and recovery. Never
-- expose it through the account snapshot API, even to the first registrant.
ALTER TABLE reflections RENAME TO legacy_reflections;
CREATE TABLE reflections (
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
    week_start TEXT NOT NULL
        CHECK (length(week_start) = 10 AND date(week_start, '+0 days') IS NOT NULL
            AND date(week_start, '+0 days') = week_start AND strftime('%w', week_start) = '1'),
    worked TEXT NOT NULL,
    obstacles TEXT NOT NULL,
    adjustment TEXT NOT NULL,
    PRIMARY KEY (user_id, week_start)
);
