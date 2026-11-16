-- Each local calendar day has one explicit tier, never a toggle or a streak.
-- Domain validation handles IANA timezones; these checks independently defend
-- the durable representation, even when an external tool writes the database.
CREATE TABLE systems (
    id TEXT PRIMARY KEY NOT NULL
        CHECK (length(id) = 36
            AND substr(id, 9, 1) = '-' AND substr(id, 14, 1) = '-'
            AND substr(id, 19, 1) = '-' AND substr(id, 24, 1) = '-'
            AND length(replace(id, '-', '')) = 32
            AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'),
    name TEXT NOT NULL CHECK (length(trim(name)) > 0),
    anchor TEXT NOT NULL CHECK (length(trim(anchor)) > 0),
    floor TEXT NOT NULL CHECK (length(trim(floor)) > 0),
    lift TEXT NOT NULL CHECK (length(trim(lift)) > 0),
    time_zone TEXT NOT NULL CHECK (length(trim(time_zone)) > 0),
    created_at TEXT NOT NULL CHECK (julianday(created_at) IS NOT NULL),
    archived INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
    momentum TEXT NOT NULL CHECK (COALESCE(
        json_valid(momentum) AND json_type(momentum) = 'object'
        AND json_type(momentum, '$.window_days') IS 'integer'
        AND json_extract(momentum, '$.window_days') BETWEEN 1 AND 365
        AND json_type(momentum, '$.floor_weight') IN ('integer', 'real')
        AND json_extract(momentum, '$.floor_weight') BETWEEN 0 AND 100
        AND json_type(momentum, '$.lift_weight') IN ('integer', 'real')
        AND json_extract(momentum, '$.lift_weight') BETWEEN 0 AND 100
        AND json_type(momentum, '$.floor_keeps_momentum') IN ('true', 'false')
        AND json_type(momentum, '$.allowed_misses') IS 'integer'
        AND json_extract(momentum, '$.allowed_misses') BETWEEN 0 AND 365, 0))
);

CREATE TABLE entries (
    system_id TEXT NOT NULL REFERENCES systems(id) ON DELETE RESTRICT,
    day TEXT NOT NULL CHECK (length(day) = 10 AND date(day, '+0 days') IS NOT NULL AND date(day, '+0 days') = day),
    tier TEXT NOT NULL CHECK (tier IN ('floor', 'lift')),
    logged_at TEXT NOT NULL CHECK (julianday(logged_at) IS NOT NULL),
    PRIMARY KEY (system_id, day)
);
CREATE INDEX entries_by_day ON entries(day, system_id);

CREATE TABLE reflections (
    week_start TEXT PRIMARY KEY NOT NULL
        CHECK (length(week_start) = 10 AND date(week_start, '+0 days') IS NOT NULL
            AND date(week_start, '+0 days') = week_start AND strftime('%w', week_start) = '1'),
    worked TEXT NOT NULL,
    obstacles TEXT NOT NULL,
    adjustment TEXT NOT NULL
);

CREATE TABLE app_metadata (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
