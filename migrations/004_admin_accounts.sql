ALTER TABLE users ADD COLUMN is_admin INTEGER NOT NULL DEFAULT 0
    CHECK (is_admin IN (0, 1));

-- Existing installations keep their accounts. The earliest account owns setup;
-- rowid breaks ties between accounts created within the same second.
UPDATE users SET is_admin = 1
WHERE id = (SELECT id FROM users ORDER BY created_at, rowid LIMIT 1);
