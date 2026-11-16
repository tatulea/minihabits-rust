-- Authentication now uses axum-login and tower-sessions. Old token sessions are
-- derived login state and cannot be converted into library sessions safely.
-- Everyone must sign in again after this migration. Accounts, password hashes,
-- system ownership, check-ins, reflections, and legacy recovery data stay intact.
-- The library creates and maintains its own session table separately at startup.
DROP TABLE sessions;
