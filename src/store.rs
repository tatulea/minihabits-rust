//! The complete persistence boundary: callers ask for habit operations, never SQL.
//!
//! One connection serializes this multi-user application's short transactions.
//! Its mutex is acquired only on Tokio's blocking pool, so SQLite cannot stall
//! an async request worker. Derived statistics belong to `domain`, not the DB.

use crate::domain::{
    Entry, MomentumConfig, Reflection, System, SystemDraft, Tier, day_in_timezone, parse_time_zone,
};
use chrono::{DateTime, Days, NaiveDate, TimeZone, Utc};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, types::Type};
use serde::{Deserialize, Serialize};
use std::{
    error::Error,
    fmt,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 4;
const ADMIN_SCHEMA: &str = include_str!("../migrations/004_admin_accounts.sql");
const LIBRARY_SESSIONS_SCHEMA: &str = include_str!("../migrations/003_library_sessions.sql");
const AUTH_SCHEMA: &str = include_str!("../migrations/002_auth.sql");
const INITIAL_SCHEMA: &str = include_str!("../migrations/001_init.sql");

/// Includes archived systems and their history so restoring a system loses nothing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub systems: Vec<System>,
    pub entries: Vec<Entry>,
    pub reflections: Vec<Reflection>,
}

#[derive(Debug)]
pub enum StoreError {
    InvalidInput(String),
    NotFound,
    AccountExists,
    RegistrationClosed,
    AdminRequired,
    Archived,
    Database(rusqlite::Error),
    Io(std::io::Error),
    Worker(String),
    UnsupportedSchema(i64),
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::AccountExists => {
                formatter.write_str("Unable to create this account. Try another username.")
            }
            Self::RegistrationClosed => formatter
                .write_str("Setup is complete. Ask an administrator to create your account."),
            Self::AdminRequired => formatter.write_str("Administrator access is required."),
            Self::NotFound => formatter.write_str("This system no longer exists."),
            Self::Archived => formatter.write_str("Restore this system before checking in."),
            Self::Database(error) => {
                write!(formatter, "Could not access the habit database: {error}")
            }
            Self::Io(error) => write!(formatter, "Could not open the habit database: {error}"),
            Self::Worker(message) => {
                write!(formatter, "The habit database is unavailable: {message}")
            }
            Self::UnsupportedSchema(version) => write!(
                formatter,
                "Database version {version} is newer than this app supports ({SCHEMA_VERSION})."
            ),
        }
    }
}

impl Error for StoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// Clones share a connection; each operation is atomic and safe to retry.
#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

impl Store {
    /// Open a local database and migrate it atomically. This is startup-only I/O.
    /// The default database is empty; sample data requires `seed_demo` explicitly.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if path.as_os_str().is_empty() {
            return Err(StoreError::InvalidInput(
                "A database path is required.".into(),
            ));
        }
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(StoreError::Io)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    pub async fn snapshot(&self, user_id: String) -> Result<Snapshot, StoreError> {
        self.with_connection(move |connection| {
            // A read transaction keeps all three collections from the same instant,
            // including when a second app process has this database open.
            let transaction = connection.transaction()?;
            let systems = transaction
                .prepare("SELECT id, name, anchor, floor, lift, time_zone, created_at, archived, momentum FROM systems WHERE user_id = ?1 ORDER BY created_at, id")?
                .query_map([&user_id], read_system)?.collect::<Result<Vec<_>, _>>()?;
            let entries = transaction
                .prepare("SELECT e.system_id, e.day, e.tier, e.logged_at FROM entries e JOIN systems s ON s.id = e.system_id WHERE s.user_id = ?1 ORDER BY e.day, e.system_id")?
                .query_map([&user_id], read_entry)?.collect::<Result<Vec<_>, _>>()?;
            let reflections = transaction
                .prepare("SELECT week_start, worked, obstacles, adjustment FROM reflections WHERE user_id = ?1 ORDER BY week_start")?
                .query_map([&user_id], read_reflection)?.collect::<Result<Vec<_>, _>>()?;
            transaction.commit()?;
            Ok(Snapshot { systems, entries, reflections })
        }).await
    }

    /// A caller-generated UUID makes a repeated create submission an update,
    /// not a duplicate. Edits never change a system's calendar or its history.
    pub async fn save_system(
        &self,
        user_id: String,
        id: String,
        draft: SystemDraft,
        time_zone: String,
    ) -> Result<(), StoreError> {
        let id = canonical_id(&id)?;
        let draft = draft
            .normalized()
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        let now = Utc::now();
        day_in_timezone(now, &time_zone)
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        let momentum = serde_json::to_string(&MomentumConfig::default())
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        self.with_connection(move |connection| {
            let changed = connection.execute(
                "INSERT INTO systems (id, name, anchor, floor, lift, time_zone, created_at, archived, momentum, user_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9) \
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, anchor = excluded.anchor, floor = excluded.floor, lift = excluded.lift \
                 WHERE systems.user_id = excluded.user_id",
                params![id, draft.name, draft.anchor, draft.floor, draft.lift, time_zone, now.to_rfc3339(), momentum, user_id],
            )?;
            if changed == 0 { return Err(StoreError::NotFound); }
            Ok(())
        }).await
    }

    /// Set today's tier in the system's saved timezone. `None` explicitly undoes
    /// today's check-in. A repeated click is a no-op, never a toggle.
    /// `expected_day` protects a tab left open across the local midnight boundary.
    pub async fn check_in(
        &self,
        user_id: String,
        system_id: String,
        tier: Option<Tier>,
        expected_day: NaiveDate,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let system_id = canonical_id(&system_id)?;
        self.with_connection(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (time_zone, archived): (String, bool) = transaction.query_row(
                "SELECT time_zone, archived FROM systems WHERE id = ?1 AND user_id = ?2", params![system_id, user_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?.ok_or(StoreError::NotFound)?;
            if archived { return Err(StoreError::Archived); }
            let day = day_in_timezone(now, &time_zone).map_err(|error| StoreError::InvalidInput(error.to_string()))?;
            if day != expected_day {
                return Err(StoreError::InvalidInput("The day changed. Refreshing Today; please log again.".into()));
            }
            let day = day.to_string();
            match tier {
                Some(tier) => {
                    transaction.execute(
                        "INSERT INTO entries (system_id, day, tier, logged_at) VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT(system_id, day) DO UPDATE SET tier = excluded.tier, logged_at = excluded.logged_at \
                         WHERE entries.tier <> excluded.tier",
                        params![system_id, day, tier.as_str(), now.to_rfc3339()],
                    )?;
                }
                None => { transaction.execute("DELETE FROM entries WHERE system_id = ?1 AND day = ?2", params![system_id, day])?; }
            }
            transaction.commit()?;
            Ok(())
        }).await
    }

    /// Archiving only hides the system; restoring it preserves every check-in.
    pub async fn archive_system(
        &self,
        user_id: String,
        id: String,
        archived: bool,
    ) -> Result<(), StoreError> {
        let id = canonical_id(&id)?;
        self.with_connection(move |connection| {
            if connection.execute(
                "UPDATE systems SET archived = ?2 WHERE id = ?1 AND user_id = ?3",
                params![id, archived, user_id],
            )? == 0
            {
                return Err(StoreError::NotFound);
            }
            Ok(())
        })
        .await
    }

    pub async fn save_reflection(
        &self,
        user_id: String,
        reflection: Reflection,
    ) -> Result<(), StoreError> {
        reflection
            .validate()
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        self.with_connection(move |connection| {
            connection.execute(
                "INSERT INTO reflections (week_start, worked, obstacles, adjustment, user_id) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(user_id, week_start) DO UPDATE SET worked = excluded.worked, obstacles = excluded.obstacles, adjustment = excluded.adjustment",
                params![reflection.week_start.to_string(), reflection.worked, reflection.obstacles, reflection.adjustment, user_id],
            )?;
            Ok(())
        }).await
    }

    /// Explicit opt-in example data. Returns true only when data was inserted.
    /// A durable marker and the empty-store check prevent surprise reseeding.
    pub async fn seed_demo(
        &self,
        user_id: String,
        now: DateTime<Utc>,
        time_zone: String,
    ) -> Result<bool, StoreError> {
        let today = day_in_timezone(now, &time_zone)
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        let zone = parse_time_zone(&time_zone)
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        let first_day = today
            .checked_sub_days(Days::new(29))
            .ok_or_else(|| StoreError::InvalidInput("Demo dates are out of range.".into()))?;
        let created_at = local_noon(first_day, zone)?;
        let momentum = serde_json::to_string(&MomentumConfig::default())
            .map_err(|error| StoreError::InvalidInput(error.to_string()))?;
        self.with_connection(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let has_data: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM systems WHERE user_id = ?1) OR EXISTS(SELECT 1 FROM reflections WHERE user_id = ?1) \
                 OR EXISTS(SELECT 1 FROM app_metadata WHERE key = 'demo_seeded:' || ?1)", [&user_id], |row| row.get(0),
            )?;
            if has_data { return Ok(false); }
            let examples = [
                ("7d20e1a8-02d0-44fa-a552-7f67d2590a01", "Move a little", "After morning coffee", "Show up and stretch", "Walk for twenty minutes"),
                ("7d20e1a8-02d0-44fa-a552-7f67d2590a02", "Read a little", "Before bed", "Read one page", "Read for twenty minutes"),
                ("7d20e1a8-02d0-44fa-a552-7f67d2590a03", "Make room to write", "After you sit at your desk", "Write one bad sentence", "Write for twenty minutes"),
            ];
            for (index, (_example_id, name, anchor, floor, lift)) in examples.into_iter().enumerate() {
                let id = Uuid::new_v4().to_string();
                transaction.execute(
                    "INSERT INTO systems (id, name, anchor, floor, lift, time_zone, created_at, archived, momentum, user_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, ?9)",
                    params![id, name, anchor, floor, lift, time_zone, created_at.to_rfc3339(), momentum, user_id],
                )?;
                // Synthetic, deterministic history. Leave today open for trying
                // the app, and show a mix of floors, lifts, and ordinary misses.
                for days_ago in 1..30_u64 {
                    if (days_ago + index as u64).is_multiple_of(5 + index as u64) { continue; }
                    let day = today.checked_sub_days(Days::new(days_ago)).ok_or_else(|| StoreError::InvalidInput("Demo dates are out of range.".into()))?;
                    let tier = if (days_ago + index as u64).is_multiple_of(3) { Tier::Lift } else { Tier::Floor };
                    let logged_at = local_noon(day, zone)?;
                    transaction.execute(
                        "INSERT INTO entries (system_id, day, tier, logged_at) VALUES (?1, ?2, ?3, ?4)",
                        params![id, day.to_string(), tier.as_str(), logged_at.to_rfc3339()],
                    )?;
                }
            }
            transaction.execute("INSERT INTO app_metadata (key, value) VALUES ('demo_seeded:' || ?1, '1')", [&user_id])?;
            transaction.commit()?;
            Ok(true)
        }).await
    }

    /// Operator-only migration, never called by registration or an HTTP endpoint.
    /// All v1 systems/history and reflections move together or not at all. An
    /// overlapping destination reflection rejects the import instead of losing
    /// either text. The legacy reflection table remains as a recovery copy.
    pub async fn import_legacy(&self, username: String) -> Result<(), StoreError> {
        self.with_connection(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let claimed: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM app_metadata WHERE key = 'legacy_imported_by')", [], |row| row.get(0),
            )?;
            if claimed {
                return Err(StoreError::InvalidInput("Legacy data has already been imported.".into()));
            }
            let user_id: String = transaction.query_row(
                "SELECT id FROM users WHERE username = ?1", [username.trim().to_ascii_lowercase()], |row| row.get(0),
            ).optional()?.ok_or_else(|| StoreError::InvalidInput("Register the destination account first.".into()))?;
            let has_legacy: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM systems WHERE user_id IS NULL) OR EXISTS(SELECT 1 FROM legacy_reflections)", [], |row| row.get(0),
            )?;
            if !has_legacy {
                return Err(StoreError::InvalidInput("There is no legacy data to import.".into()));
            }
            let collision: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM reflections r JOIN legacy_reflections l USING (week_start) WHERE r.user_id = ?1)", [&user_id], |row| row.get(0),
            )?;
            if collision {
                return Err(StoreError::InvalidInput("This account already has a reflection for a legacy week. Import into an account without overlapping reflections.".into()));
            }
            transaction.execute("UPDATE systems SET user_id = ?1 WHERE user_id IS NULL", [&user_id])?;
            transaction.execute(
                "INSERT INTO reflections (user_id, week_start, worked, obstacles, adjustment) SELECT ?1, week_start, worked, obstacles, adjustment FROM legacy_reflections", [&user_id],
            )?;
            transaction.execute("INSERT INTO app_metadata (key, value) VALUES ('legacy_imported_by', ?1)", [&user_id])?;
            transaction.commit()?;
            Ok(())
        }).await
    }

    pub(crate) async fn credentials(
        &self,
        username: String,
    ) -> Result<Option<(crate::auth::User, String)>, StoreError> {
        self.with_connection(move |connection| {
            Ok(connection
                .query_row(
                    "SELECT id, username, password_hash, is_admin FROM users WHERE username = ?1",
                    [username],
                    |row| {
                        Ok((
                            crate::auth::User {
                                id: row.get(0)?,
                                username: row.get(1)?,
                                is_admin: row.get(3)?,
                            },
                            row.get(2)?,
                        ))
                    },
                )
                .optional()?)
        })
        .await
    }

    pub async fn registration_open(&self) -> Result<bool, StoreError> {
        self.with_connection(|connection| {
            Ok(
                connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM users)", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
    }

    pub async fn users_for_admin(
        &self,
        admin_id: String,
    ) -> Result<Vec<crate::auth::User>, StoreError> {
        self.with_connection(move |connection| {
            let transaction = connection.transaction()?;
            require_admin(&transaction, &admin_id)?;
            let users = transaction
                .prepare("SELECT id, username, is_admin FROM users ORDER BY username")?
                .query_map([], |row| {
                    Ok(crate::auth::User {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        is_admin: row.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            transaction.commit()?;
            Ok(users)
        })
        .await
    }

    /// Only the first public signup can become an administrator. Check setup
    /// and administrator permissions under the same lock as the insert.
    pub(crate) async fn create_account(
        &self,
        mut user: crate::auth::User,
        password_hash: String,
        admin_id: Option<String>,
    ) -> Result<crate::auth::User, StoreError> {
        self.with_connection(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            user.is_admin = if let Some(admin_id) = admin_id {
                require_admin(&transaction, &admin_id)?;
                false
            } else {
                let exists: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |row| row.get(0))?;
                if exists { return Err(StoreError::RegistrationClosed); }
                true
            };
            // ON CONFLICT(username), rather than a catch-all constraint handler,
            // makes concurrent duplicate registration atomic without swallowing
            // disk failures or a malformed credential representation.
            if transaction.execute(
                "INSERT INTO users (id, username, password_hash, created_at, is_admin) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(username) DO NOTHING",
                params![user.id, user.username, password_hash, Utc::now().timestamp(), user.is_admin],
            )? == 0 { return Err(StoreError::AccountExists); }
            transaction.commit()?;
            Ok(user)
        }).await
    }

    /// Reload the account and its current password hash for the authentication
    /// backend. Session persistence and invalidation belong to the session library.
    pub(crate) async fn credentials_by_id(
        &self,
        user_id: String,
    ) -> Result<Option<(crate::auth::User, String)>, StoreError> {
        self.with_connection(move |connection| {
            Ok(connection
                .query_row(
                    "SELECT id, username, password_hash, is_admin FROM users WHERE id = ?1",
                    [user_id],
                    |row| {
                        Ok((
                            crate::auth::User {
                                id: row.get(0)?,
                                username: row.get(1)?,
                                is_admin: row.get(3)?,
                            },
                            row.get(2)?,
                        ))
                    },
                )
                .optional()?)
        })
        .await
    }

    async fn with_connection<T, F>(&self, operation: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| StoreError::Worker("The database lock was interrupted.".into()))?;
            operation(&mut connection)
        })
        .await
        .map_err(|error| StoreError::Worker(error.to_string()))?
    }
}

fn require_admin(connection: &Connection, user_id: &str) -> Result<(), StoreError> {
    let is_admin: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM users WHERE id = ?1 AND is_admin = 1)",
        [user_id],
        |row| row.get(0),
    )?;
    if is_admin {
        Ok(())
    } else {
        Err(StoreError::AdminRequired)
    }
}

/// Calendar arithmetic must stay local across DST. Noon avoids ordinary clock
/// changes; a wholly skipped civil date is rejected rather than mislabeled.
fn local_noon(day: NaiveDate, zone: chrono_tz::Tz) -> Result<DateTime<Utc>, StoreError> {
    zone.from_local_datetime(&day.and_hms_opt(12, 0, 0).expect("noon is a valid time"))
        .earliest()
        .map(|date| date.with_timezone(&Utc))
        .ok_or_else(|| {
            StoreError::InvalidInput("A demo date does not exist in this timezone.".into())
        })
}

fn migrate(connection: &mut Connection) -> Result<(), StoreError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchema(version));
    }
    if version == 0 {
        transaction.execute_batch(INITIAL_SCHEMA)?;
    }
    if version < 2 {
        transaction.execute_batch(AUTH_SCHEMA)?;
    }
    if version < 3 {
        transaction.execute_batch(LIBRARY_SESSIONS_SCHEMA)?;
    }
    if version < 4 {
        transaction.execute_batch(ADMIN_SCHEMA)?;
    }
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn canonical_id(id: &str) -> Result<String, StoreError> {
    Uuid::parse_str(id)
        .map(|id| id.hyphenated().to_string())
        .map_err(|_| StoreError::InvalidInput("A valid system ID is required.".into()))
}

fn conversion_error(index: usize, error: impl Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

fn read_date(row: &Row<'_>, index: usize) -> rusqlite::Result<NaiveDate> {
    let value: String = row.get(index)?;
    NaiveDate::parse_from_str(&value, "%Y-%m-%d").map_err(|error| conversion_error(index, error))
}

fn read_timestamp(row: &Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    let value: String = row.get(index)?;
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| conversion_error(index, error))
}

fn read_system(row: &Row<'_>) -> rusqlite::Result<System> {
    let momentum: String = row.get(8)?;
    Ok(System {
        id: row.get(0)?,
        name: row.get(1)?,
        anchor: row.get(2)?,
        floor: row.get(3)?,
        lift: row.get(4)?,
        time_zone: row.get(5)?,
        created_at: read_timestamp(row, 6)?,
        archived: row.get(7)?,
        momentum: serde_json::from_str(&momentum).map_err(|error| conversion_error(8, error))?,
    })
}

fn read_entry(row: &Row<'_>) -> rusqlite::Result<Entry> {
    let tier: String = row.get(2)?;
    Ok(Entry {
        system_id: row.get(0)?,
        day: read_date(row, 1)?,
        tier: tier.parse().map_err(|error| conversion_error(2, error))?,
        logged_at: read_timestamp(row, 3)?,
    })
}

fn read_reflection(row: &Row<'_>) -> rusqlite::Result<Reflection> {
    Ok(Reflection {
        week_start: read_date(row, 0)?,
        worked: row.get(1)?,
        obstacles: row.get(2)?,
        adjustment: row.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    const USER_ID: &str = "8c03016b-e039-4d7c-8971-193b3daf6ffb";

    fn database() -> (TempDir, Store) {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("habits.sqlite3")).unwrap();
        store.connection.lock().unwrap().execute(
            "INSERT INTO users (id, username, password_hash, created_at, is_admin) VALUES (?1, 'owner', '$argon2id$test-only', 0, 1)",
            [USER_ID],
        ).unwrap();
        (directory, store)
    }

    fn draft() -> SystemDraft {
        SystemDraft {
            name: "  Read a little  ".into(),
            anchor: " Before bed ".into(),
            floor: " Read one page ".into(),
            lift: " Read for twenty minutes ".into(),
        }
    }

    fn instant() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 21, 0, 30, 0).unwrap()
    }

    async fn add_system(store: &Store, time_zone: &str) -> String {
        let id = Uuid::new_v4().to_string();
        store
            .save_system(USER_ID.into(), id.clone(), draft(), time_zone.into())
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn new_database_is_empty_and_configured() {
        let (_directory, store) = database();
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert!(snapshot.systems.is_empty());
        assert!(snapshot.entries.is_empty());
        assert!(snapshot.reflections.is_empty());
        store
            .with_connection(|connection| {
                let version: i64 =
                    connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
                let foreign_keys: bool =
                    connection.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
                let journal: String =
                    connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
                let timeout: i64 =
                    connection.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
                assert_eq!(version, SCHEMA_VERSION);
                assert!(foreign_keys);
                assert_eq!(journal, "wal");
                assert_eq!(timeout, 5000);
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn reopening_preserves_system_entries_and_reflections() {
        let (directory, store) = database();
        let id = add_system(&store, "UTC").await;
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                instant().date_naive(),
                instant(),
            )
            .await
            .unwrap();
        let reflection = Reflection {
            week_start: NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(),
            worked: "Leaving the book out".into(),
            obstacles: "Late evenings".into(),
            adjustment: "Read after dinner".into(),
        };
        store
            .save_reflection(USER_ID.into(), reflection.clone())
            .await
            .unwrap();
        drop(store);
        let store = Store::open(&directory.path().join("habits.sqlite3")).unwrap();
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(snapshot.systems.len(), 1);
        assert_eq!(snapshot.systems[0].id, id);
        assert_eq!(snapshot.systems[0].name, "Read a little");
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].tier, Tier::Floor);
        assert_eq!(snapshot.reflections, vec![reflection]);
    }

    #[tokio::test]
    async fn upsert_preserves_identity_timezone_creation_archive_and_momentum() {
        let (_directory, store) = database();
        let id = add_system(&store, "America/Los_Angeles").await;
        store
            .archive_system(USER_ID.into(), id.clone(), true)
            .await
            .unwrap();
        let original = store
            .snapshot(USER_ID.into())
            .await
            .unwrap()
            .systems
            .remove(0);
        let mut changed = draft();
        changed.floor = "Read one paragraph".into();
        store
            .save_system(
                USER_ID.into(),
                id.to_uppercase(),
                changed.clone(),
                "Asia/Tokyo".into(),
            )
            .await
            .unwrap();
        store
            .save_system(USER_ID.into(), id, changed, "UTC".into())
            .await
            .unwrap();
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(snapshot.systems.len(), 1);
        let updated = &snapshot.systems[0];
        assert_eq!(updated.floor, "Read one paragraph");
        assert_eq!(updated.time_zone, original.time_zone);
        assert_eq!(updated.created_at, original.created_at);
        assert_eq!(updated.momentum, original.momentum);
        assert!(updated.archived);
    }

    #[tokio::test]
    async fn repeated_check_ins_are_sets_and_use_the_system_calendar() {
        let (_directory, store) = database();
        let id = add_system(&store, "America/Los_Angeles").await;
        let now = instant();
        let expected_day = day_in_timezone(now, "America/Los_Angeles").unwrap();
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                expected_day,
                now,
            )
            .await
            .unwrap();
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                expected_day,
                now + chrono::Duration::minutes(1),
            )
            .await
            .unwrap();
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].day,
            NaiveDate::from_ymd_opt(2026, 10, 20).unwrap()
        );
        assert_eq!(snapshot.entries[0].logged_at, now);
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Lift),
                expected_day,
                now + chrono::Duration::minutes(2),
            )
            .await
            .unwrap();
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].tier, Tier::Lift);
        store
            .check_in(USER_ID.into(), id.clone(), None, expected_day, now)
            .await
            .unwrap();
        store
            .check_in(USER_ID.into(), id, None, expected_day, now)
            .await
            .unwrap();
        assert!(
            store
                .snapshot(USER_ID.into())
                .await
                .unwrap()
                .entries
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_connections_cannot_duplicate_a_days_check_in() {
        let (directory, first) = database();
        let id = add_system(&first, "UTC").await;
        let second = Store::open(&directory.path().join("habits.sqlite3")).unwrap();
        let (a, b) = tokio::join!(
            first.check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                instant().date_naive(),
                instant()
            ),
            second.check_in(
                USER_ID.into(),
                id,
                Some(Tier::Floor),
                instant().date_naive(),
                instant()
            )
        );
        a.unwrap();
        b.unwrap();
        assert_eq!(
            first.snapshot(USER_ID.into()).await.unwrap().entries.len(),
            1
        );
    }

    #[tokio::test]
    async fn archive_and_restore_are_idempotent_and_preserve_history() {
        let (_directory, store) = database();
        let id = add_system(&store, "UTC").await;
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                instant().date_naive(),
                instant(),
            )
            .await
            .unwrap();
        store
            .archive_system(USER_ID.into(), id.clone(), true)
            .await
            .unwrap();
        store
            .archive_system(USER_ID.into(), id.clone(), true)
            .await
            .unwrap();
        assert!(matches!(
            store
                .check_in(
                    USER_ID.into(),
                    id.clone(),
                    Some(Tier::Lift),
                    instant().date_naive(),
                    instant()
                )
                .await,
            Err(StoreError::Archived)
        ));
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert!(snapshot.systems[0].archived);
        assert_eq!(snapshot.entries.len(), 1);
        store
            .archive_system(USER_ID.into(), id.clone(), false)
            .await
            .unwrap();
        store
            .archive_system(USER_ID.into(), id.clone(), false)
            .await
            .unwrap();
        store
            .check_in(
                USER_ID.into(),
                id,
                Some(Tier::Lift),
                instant().date_naive(),
                instant(),
            )
            .await
            .unwrap();
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert!(!snapshot.systems[0].archived);
        assert_eq!(snapshot.entries[0].tier, Tier::Lift);
    }

    #[tokio::test]
    async fn unknown_ids_and_invalid_input_fail_without_mutation() {
        let (_directory, store) = database();
        let unknown = Uuid::new_v4().to_string();
        assert!(matches!(
            store
                .check_in(
                    USER_ID.into(),
                    unknown.clone(),
                    Some(Tier::Floor),
                    instant().date_naive(),
                    instant()
                )
                .await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store
                .check_in(
                    USER_ID.into(),
                    unknown.clone(),
                    None,
                    instant().date_naive(),
                    instant()
                )
                .await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store
                .archive_system(USER_ID.into(), unknown.clone(), true)
                .await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store
                .save_system(USER_ID.into(), "bad id".into(), draft(), "UTC".into())
                .await,
            Err(StoreError::InvalidInput(_))
        ));
        assert!(matches!(
            store
                .save_system(
                    USER_ID.into(),
                    unknown.clone(),
                    SystemDraft::default(),
                    "UTC".into()
                )
                .await,
            Err(StoreError::InvalidInput(_))
        ));
        assert!(matches!(
            store
                .save_system(USER_ID.into(), unknown, draft(), "Not/A_Zone".into())
                .await,
            Err(StoreError::InvalidInput(_))
        ));
        assert!(
            store
                .snapshot(USER_ID.into())
                .await
                .unwrap()
                .systems
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reflection_upsert_preserves_exact_text_and_rejects_non_monday() {
        let (_directory, store) = database();
        let week_start = NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
        store
            .save_reflection(USER_ID.into(), Reflection::empty(week_start))
            .await
            .unwrap();
        let mut reflection = Reflection::empty(week_start);
        reflection.worked = "  A small start\nReally helped.  ".into();
        store
            .save_reflection(USER_ID.into(), reflection.clone())
            .await
            .unwrap();
        store
            .save_reflection(USER_ID.into(), reflection.clone())
            .await
            .unwrap();
        assert_eq!(
            store.snapshot(USER_ID.into()).await.unwrap().reflections,
            vec![reflection.clone()]
        );
        reflection.week_start = week_start.succ_opt().unwrap();
        assert!(matches!(
            store.save_reflection(USER_ID.into(), reflection).await,
            Err(StoreError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn demo_is_explicit_and_never_reseeds_after_archiving_or_reopening() {
        let (directory, store) = database();
        assert!(
            store
                .seed_demo(USER_ID.into(), instant(), "America/Los_Angeles".into())
                .await
                .unwrap()
        );
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(snapshot.systems.len(), 3);
        assert!(snapshot.entries.len() > 60);
        for system in &snapshot.systems {
            store
                .archive_system(USER_ID.into(), system.id.clone(), true)
                .await
                .unwrap();
        }
        drop(store);
        let store = Store::open(&directory.path().join("habits.sqlite3")).unwrap();
        assert!(
            !store
                .seed_demo(USER_ID.into(), instant(), "UTC".into())
                .await
                .unwrap()
        );
        let after = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(after.systems.len(), 3);
        assert!(after.systems.iter().all(|system| system.archived));
        assert_eq!(after.entries, snapshot.entries);
    }

    #[tokio::test]
    async fn demo_does_not_mix_sample_data_into_a_personal_database() {
        let (_directory, store) = database();
        add_system(&store, "UTC").await;
        assert!(
            !store
                .seed_demo(USER_ID.into(), instant(), "UTC".into())
                .await
                .unwrap()
        );
        assert_eq!(
            store.snapshot(USER_ID.into()).await.unwrap().systems.len(),
            1
        );
    }

    #[tokio::test]
    async fn a_stale_tab_cannot_log_or_undo_a_different_local_day() {
        let (_directory, store) = database();
        let id = add_system(&store, "America/New_York").await;
        let before_midnight = Utc.with_ymd_and_hms(2026, 10, 21, 3, 59, 0).unwrap();
        let after_midnight = Utc.with_ymd_and_hms(2026, 10, 21, 4, 1, 0).unwrap();
        let old_day = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let new_day = old_day.succ_opt().unwrap();
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                old_day,
                before_midnight,
            )
            .await
            .unwrap();
        let error = store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Lift),
                old_day,
                after_midnight,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "The day changed. Refreshing Today; please log again."
        );
        assert!(matches!(error, StoreError::InvalidInput(_)));
        let entries = store.snapshot(USER_ID.into()).await.unwrap().entries;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].day, old_day);
        assert_eq!(entries[0].tier, Tier::Floor);

        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Lift),
                new_day,
                after_midnight,
            )
            .await
            .unwrap();
        assert!(matches!(
            store
                .check_in(USER_ID.into(), id, None, old_day, after_midnight)
                .await,
            Err(StoreError::InvalidInput(_))
        ));
        let entries = store.snapshot(USER_ID.into()).await.unwrap().entries;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].day, new_day);
        assert_eq!(entries[1].tier, Tier::Lift);
    }

    #[tokio::test]
    async fn demo_timestamps_match_calendar_days_across_daylight_saving_time() {
        let (_directory, store) = database();
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 4, 30, 0).unwrap();
        let zone = "America/New_York";
        let today = day_in_timezone(now, zone).unwrap();
        assert!(
            store
                .seed_demo(USER_ID.into(), now, zone.into())
                .await
                .unwrap()
        );
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert!(!snapshot.entries.is_empty());
        for entry in &snapshot.entries {
            assert_eq!(day_in_timezone(entry.logged_at, zone).unwrap(), entry.day);
            assert!(entry.day < today);
        }
        for system in &snapshot.systems {
            assert_eq!(
                day_in_timezone(system.created_at, zone).unwrap(),
                today.checked_sub_days(Days::new(29)).unwrap()
            );
        }
    }

    #[tokio::test]
    async fn sql_constraints_reject_invalid_rows_and_preserve_foreign_keys() {
        let (_directory, store) = database();
        let id = add_system(&store, "UTC").await;
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                instant().date_naive(),
                instant(),
            )
            .await
            .unwrap();
        store.with_connection(move |connection| {
            for sql in [
                "UPDATE systems SET name = ''",
                "UPDATE systems SET archived = 2",
                "UPDATE systems SET created_at = 'not a timestamp'",
                "UPDATE systems SET momentum = '{}'",
                "UPDATE systems SET momentum = json_remove(momentum, '$.floor_weight')",
                "UPDATE systems SET momentum = json_set(momentum, '$.window_days', 0)",
                "UPDATE systems SET momentum = json_set(momentum, '$.lift_weight', -1)",
                "UPDATE entries SET tier = 'miss'",
                "UPDATE entries SET day = '2026-02-29'",
                "UPDATE entries SET logged_at = 'yesterday'",
                "UPDATE entries SET system_id = 'missing'",
                "DELETE FROM systems",
                "INSERT INTO reflections VALUES ('8c03016b-e039-4d7c-8971-193b3daf6ffb', '2026-10-20', '', '', '')",
            ] {
                assert!(connection.execute(sql, []).is_err(), "Constraint accepted: {sql}");
            }
            assert!(connection.execute(
                "INSERT INTO entries (system_id, day, tier, logged_at) VALUES (?1, '2026-10-21', 'lift', '2026-10-21T12:00:00Z')",
                [id],
            ).is_err());
            Ok(())
        }).await.unwrap();
    }

    #[tokio::test]
    async fn user_text_is_data_not_sql() {
        let (_directory, store) = database();
        let mut draft = draft();
        draft.name = "Read'); DROP TABLE systems; --".into();
        store
            .save_system(
                USER_ID.into(),
                Uuid::new_v4().to_string(),
                draft.clone(),
                "UTC".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            store.snapshot(USER_ID.into()).await.unwrap().systems[0].name,
            draft.name
        );
    }

    fn add_other_user(store: &Store) -> String {
        let id = Uuid::new_v4().to_string();
        store.connection.lock().unwrap().execute(
            "INSERT INTO users (id, username, password_hash, created_at) VALUES (?1, 'other', '$argon2id$test-only', 0)", [&id],
        ).unwrap();
        id
    }

    #[tokio::test]
    async fn account_boundaries_cover_reads_writes_and_forged_uuid_upserts() {
        let (_directory, store) = database();
        let other = add_other_user(&store);
        let id = add_system(&store, "UTC").await;
        let reflection = Reflection::empty(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        store
            .save_reflection(USER_ID.into(), reflection.clone())
            .await
            .unwrap();
        store
            .check_in(
                USER_ID.into(),
                id.clone(),
                Some(Tier::Floor),
                instant().date_naive(),
                instant(),
            )
            .await
            .unwrap();
        let before = store.snapshot(USER_ID.into()).await.unwrap();
        let empty = store.snapshot(other.clone()).await.unwrap();
        assert!(
            empty.systems.is_empty() && empty.entries.is_empty() && empty.reflections.is_empty()
        );
        assert!(matches!(
            store
                .save_system(other.clone(), id.clone(), draft(), "UTC".into())
                .await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.archive_system(other.clone(), id.clone(), true).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store
                .check_in(
                    other.clone(),
                    id.clone(),
                    Some(Tier::Lift),
                    instant().date_naive(),
                    instant()
                )
                .await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store
                .check_in(
                    other.clone(),
                    id.clone(),
                    None,
                    instant().date_naive(),
                    instant()
                )
                .await,
            Err(StoreError::NotFound)
        ));
        let mut other_reflection = reflection.clone();
        other_reflection.worked = "Only the second account can see this".into();
        store
            .save_reflection(other.clone(), other_reflection.clone())
            .await
            .unwrap();
        let after = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(before.systems, after.systems);
        assert_eq!(before.entries, after.entries);
        assert_eq!(before.reflections, after.reflections);
        assert_eq!(
            store.snapshot(other).await.unwrap().reflections,
            vec![other_reflection]
        );
    }

    #[tokio::test]
    async fn demo_is_per_account_and_never_uses_shared_system_ids() {
        let (_directory, store) = database();
        let other = add_other_user(&store);
        assert!(
            store
                .seed_demo(USER_ID.into(), instant(), "UTC".into())
                .await
                .unwrap()
        );
        assert!(
            store
                .seed_demo(other.clone(), instant(), "UTC".into())
                .await
                .unwrap()
        );
        let first = store.snapshot(USER_ID.into()).await.unwrap();
        let second = store.snapshot(other).await.unwrap();
        assert_eq!(first.systems.len(), 3);
        assert_eq!(second.systems.len(), 3);
        assert!(
            first
                .systems
                .iter()
                .all(|a| second.systems.iter().all(|b| a.id != b.id))
        );
    }

    fn legacy_database() -> (TempDir, Store, String) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(INITIAL_SCHEMA).unwrap();
        let id = Uuid::new_v4().to_string();
        connection.execute(
            "INSERT INTO systems (id, name, anchor, floor, lift, time_zone, created_at, archived, momentum) VALUES (?1, 'Private old habit', 'After tea', 'Read', 'Read more', 'UTC', '2026-10-01T00:00:00Z', 1, ?2)",
            params![id, serde_json::to_string(&MomentumConfig::default()).unwrap()],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO entries VALUES (?1, '2026-10-19', 'floor', '2026-10-19T12:00:00Z')",
                [&id],
            )
            .unwrap();
        connection.execute("INSERT INTO reflections VALUES ('2026-10-19', 'Private old reflection', 'Obstacle', 'Adjustment')", []).unwrap();
        connection
            .execute("INSERT INTO app_metadata VALUES ('demo_seeded', '1')", [])
            .unwrap();
        connection.pragma_update(None, "user_version", 1).unwrap();
        drop(connection);
        (directory, Store::open(&path).unwrap(), id)
    }

    #[tokio::test]
    async fn migration_never_auto_assigns_legacy_and_explicit_import_is_atomic_once() {
        let (_directory, store, old_id) = legacy_database();
        let other = add_other_user(&store);
        let no_auto_assignment = store.snapshot(other.clone()).await.unwrap();
        assert!(
            no_auto_assignment.systems.is_empty()
                && no_auto_assignment.entries.is_empty()
                && no_auto_assignment.reflections.is_empty()
        );
        assert!(matches!(
            store
                .save_system(other.clone(), old_id.clone(), draft(), "UTC".into())
                .await,
            Err(StoreError::NotFound)
        ));
        assert!(store.import_legacy("nonexistent".into()).await.is_err());
        let reflection = Reflection::empty(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap());
        store
            .save_reflection(other.clone(), reflection.clone())
            .await
            .unwrap();
        assert!(store.import_legacy("other".into()).await.is_err());
        assert!(
            store
                .snapshot(other.clone())
                .await
                .unwrap()
                .systems
                .is_empty()
        );
        store
            .with_connection(move |connection| {
                assert_eq!(
                    connection.query_row(
                        "SELECT COUNT(*) FROM systems WHERE user_id IS NULL",
                        [],
                        |row| row.get::<_, i64>(0)
                    )?,
                    1
                );
                assert_eq!(
                    connection
                        .query_row("SELECT COUNT(*) FROM legacy_reflections", [], |row| row
                            .get::<_, i64>(0))?,
                    1
                );
                connection.execute("DELETE FROM reflections", [])?;
                Ok(())
            })
            .await
            .unwrap();
        store.import_legacy("OTHER".into()).await.unwrap();
        let imported = store.snapshot(other).await.unwrap();
        assert_eq!(imported.systems.len(), 1);
        assert_eq!(imported.systems[0].id, old_id);
        assert!(imported.systems[0].archived);
        assert_eq!(imported.entries.len(), 1);
        assert_eq!(imported.reflections[0].worked, "Private old reflection");
        assert!(store.import_legacy("other".into()).await.is_err());
    }

    #[tokio::test]
    async fn account_creation_and_lookup_do_not_depend_on_session_storage() {
        let (_directory, store) = database();
        let user = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: "new_account".into(),
            is_admin: false,
        };
        let password_hash = "$argon2id$test-only-new-account".to_string();
        store
            .create_account(user.clone(), password_hash.clone(), Some(USER_ID.into()))
            .await
            .unwrap();
        let expected = Some((user.clone(), password_hash));
        assert_eq!(
            store.credentials(user.username.clone()).await.unwrap(),
            expected
        );
        assert_eq!(
            store.credentials_by_id(user.id.clone()).await.unwrap(),
            expected
        );
        assert!(store.credentials("missing".into()).await.unwrap().is_none());
        assert!(
            store
                .credentials_by_id(Uuid::new_v4().to_string())
                .await
                .unwrap()
                .is_none()
        );

        let duplicate = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: user.username.clone(),
            is_admin: false,
        };
        assert!(matches!(
            store
                .create_account(
                    duplicate.clone(),
                    "$argon2id$replacement".into(),
                    Some(USER_ID.into())
                )
                .await,
            Err(StoreError::AccountExists)
        ));
        assert!(
            store
                .credentials_by_id(duplicate.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(store.credentials_by_id(user.id).await.unwrap(), expected);

        let invalid = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: "invalid_hash".into(),
            is_admin: false,
        };
        assert!(matches!(
            store
                .create_account(invalid.clone(), "not-a-hash".into(), Some(USER_ID.into()))
                .await,
            Err(StoreError::Database(_))
        ));
        assert!(store.credentials_by_id(invalid.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn concurrent_duplicate_account_creation_creates_exactly_one_account() {
        let (directory, first) = database();
        let second = Store::open(&directory.path().join("habits.sqlite3")).unwrap();
        let first_user = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: "same_username".into(),
            is_admin: false,
        };
        let second_user = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: first_user.username.clone(),
            is_admin: false,
        };
        let (a, b) = tokio::join!(
            first.create_account(
                first_user.clone(),
                "$argon2id$first".into(),
                Some(USER_ID.into())
            ),
            second.create_account(
                second_user.clone(),
                "$argon2id$second".into(),
                Some(USER_ID.into())
            ),
        );
        let expected = match (a, b) {
            (Ok(user), Err(StoreError::AccountExists)) => (user, "$argon2id$first"),
            (Err(StoreError::AccountExists), Ok(user)) => (user, "$argon2id$second"),
            results => panic!("Expected one account and one duplicate rejection: {results:?}"),
        };
        assert_eq!(
            first.credentials("same_username".into()).await.unwrap(),
            Some((expected.0, expected.1.into()))
        );
    }

    #[tokio::test]
    async fn first_signup_is_atomic_across_independent_connections_and_restarts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bootstrap.sqlite3");
        let first = Store::open(&path).unwrap();
        let second = Store::open(&path).unwrap();
        assert!(first.registration_open().await.unwrap());
        let user = |username: &str| crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: username.into(),
            is_admin: false,
        };
        let (a, b) = tokio::join!(
            first.create_account(user("first"), "$argon2id$first".into(), None),
            second.create_account(user("second"), "$argon2id$second".into(), None),
        );
        let admin = match (a, b) {
            (Ok(user), Err(StoreError::RegistrationClosed))
            | (Err(StoreError::RegistrationClosed), Ok(user)) => user,
            results => panic!("Expected one administrator: {results:?}"),
        };
        assert!(admin.is_admin);
        let reopened = Store::open(&path).unwrap();
        assert!(!reopened.registration_open().await.unwrap());
        assert_eq!(
            reopened.users_for_admin(admin.id.clone()).await.unwrap(),
            vec![admin]
        );
        assert!(matches!(
            reopened
                .create_account(user("third"), "$argon2id$third".into(), None)
                .await,
            Err(StoreError::RegistrationClosed)
        ));
    }

    #[tokio::test]
    async fn account_creation_checks_stored_permissions_and_cannot_grant_admin() {
        let (_directory, store) = database();
        let member = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: "member".into(),
            is_admin: true,
        };
        let member = store
            .create_account(member, "$argon2id$member".into(), Some(USER_ID.into()))
            .await
            .unwrap();
        assert!(!member.is_admin);
        let target = crate::auth::User {
            id: Uuid::new_v4().to_string(),
            username: "unauthorized".into(),
            is_admin: true,
        };
        for actor in [member.id.clone(), Uuid::new_v4().to_string()] {
            assert!(matches!(
                store.users_for_admin(actor.clone()).await,
                Err(StoreError::AdminRequired)
            ));
            assert!(matches!(
                store
                    .create_account(target.clone(), "$argon2id$target".into(), Some(actor))
                    .await,
                Err(StoreError::AdminRequired)
            ));
        }
        assert!(store.credentials(target.username).await.unwrap().is_none());
        assert_eq!(
            store.users_for_admin(USER_ID.into()).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn v3_upgrade_promotes_only_the_first_account_and_preserves_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v3.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(INITIAL_SCHEMA).unwrap();
        connection.execute_batch(AUTH_SCHEMA).unwrap();
        connection.execute_batch(LIBRARY_SESSIONS_SCHEMA).unwrap();
        let first = Uuid::new_v4().to_string();
        let tied = Uuid::new_v4().to_string();
        let later = Uuid::new_v4().to_string();
        for (id, name, created) in [
            (&first, "first", 100),
            (&tied, "tied", 100),
            (&later, "later", 200),
        ] {
            connection
                .execute(
                    "INSERT INTO users VALUES (?1, ?2, '$argon2id$unchanged', ?3)",
                    params![id, name, created],
                )
                .unwrap();
        }
        // The role migration must not touch the library's session table.
        connection.execute_batch("CREATE TABLE tower_sessions (id TEXT PRIMARY KEY, data BLOB NOT NULL, expiry_date INTEGER NOT NULL); INSERT INTO tower_sessions VALUES ('existing-session', X'0102', 2000000000);").unwrap();
        connection.pragma_update(None, "user_version", 3).unwrap();
        let before = durable_rows(&connection);
        drop(connection);
        let store = Store::open(&path).unwrap();
        let accounts = store.users_for_admin(first.clone()).await.unwrap();
        assert_eq!(
            accounts
                .iter()
                .filter(|user| user.is_admin)
                .map(|user| &user.id)
                .collect::<Vec<_>>(),
            vec![&first]
        );
        assert_eq!(durable_rows(&store.connection.lock().unwrap()), before);
        assert!(!store.registration_open().await.unwrap());
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.users_for_admin(first).await.unwrap(), accounts);
        let connection = reopened.connection.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT data FROM tower_sessions WHERE id = 'existing-session'",
                    [],
                    |row| row.get::<_, Vec<u8>>(0)
                )
                .unwrap(),
            vec![1, 2]
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
    }

    /// Snapshot every pre-v4 value, including unowned legacy rows and metadata.
    /// The table names are fixed test fixtures, never user-provided SQL.
    fn durable_rows(connection: &Connection) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
        [
            "users",
            "systems",
            "entries",
            "legacy_reflections",
            "reflections",
            "app_metadata",
        ]
        .into_iter()
        .map(|table| {
            let columns = if table == "users" {
                "id, username, password_hash, created_at"
            } else {
                "*"
            };
            let mut statement = connection
                .prepare(&format!("SELECT {columns} FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns).map(|column| row.get(column)).collect()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect()
    }

    #[tokio::test]
    async fn v2_migration_invalidates_old_sessions_and_preserves_all_durable_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("v2.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        connection.execute_batch(INITIAL_SCHEMA).unwrap();
        connection.execute_batch(AUTH_SCHEMA).unwrap();
        connection
            .execute(
                "INSERT INTO users VALUES (?1, 'owner', '$argon2id$existing-password', 100)",
                [USER_ID],
            )
            .unwrap();
        let owned_id = Uuid::new_v4().to_string();
        let legacy_id = Uuid::new_v4().to_string();
        for (id, owner, archived) in [(&owned_id, Some(USER_ID), false), (&legacy_id, None, true)] {
            connection.execute(
                "INSERT INTO systems (id, name, anchor, floor, lift, time_zone, created_at, archived, momentum, user_id) VALUES (?1, 'Keep this habit', 'After tea', 'Read', 'Read more', 'UTC', '2026-10-01T00:00:00Z', ?2, ?3, ?4)",
                params![id, archived, serde_json::to_string(&MomentumConfig::default()).unwrap(), owner],
            ).unwrap();
            connection.execute(
                "INSERT INTO entries VALUES (?1, '2026-10-19', 'floor', '2026-10-19T12:00:00Z')", [id],
            ).unwrap();
        }
        connection.execute(
            "INSERT INTO reflections VALUES (?1, '2026-10-19', 'Account reflection', 'Obstacle', 'Adjustment')", [USER_ID],
        ).unwrap();
        connection.execute(
            "INSERT INTO legacy_reflections VALUES ('2026-10-19', 'Unowned reflection', 'Old obstacle', 'Old adjustment')", [],
        ).unwrap();
        connection
            .execute(
                "INSERT INTO app_metadata VALUES ('demo_seeded', '1'), ('demo_seeded:' || ?1, '1')",
                [USER_ID],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO sessions VALUES (?1, ?2, 100, 2000000000)",
                params![format!("{:064x}", 1), USER_ID],
            )
            .unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        let before = durable_rows(&connection);
        drop(connection);

        let store = Store::open(&path).unwrap();
        {
            let connection = store.connection.lock().unwrap();
            assert_eq!(durable_rows(&connection), before);
            assert_eq!(
                connection
                    .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                    .unwrap(),
                SCHEMA_VERSION
            );
            assert_eq!(
                connection.query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('sessions', 'sessions_by_user', 'sessions_by_expiry')",
                    [], |row| row.get::<_, i64>(0),
                ).unwrap(),
                0
            );
            assert!(
                connection
                    .prepare("PRAGMA foreign_key_check")
                    .unwrap()
                    .query([])
                    .unwrap()
                    .next()
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            store.credentials_by_id(USER_ID.into()).await.unwrap(),
            Some((
                crate::auth::User {
                    id: USER_ID.into(),
                    username: "owner".into(),
                    is_admin: true,
                },
                "$argon2id$existing-password".into()
            ))
        );
        let snapshot = store.snapshot(USER_ID.into()).await.unwrap();
        assert_eq!(snapshot.systems.len(), 1);
        assert_eq!(snapshot.systems[0].id, owned_id);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.reflections[0].worked, "Account reflection");
        drop(store);

        // Reopening the current schema does not repeat the destructive v2 cleanup.
        let reopened = Store::open(&path).unwrap();
        assert_eq!(durable_rows(&reopened.connection.lock().unwrap()), before);
    }

    #[test]
    fn store_is_safe_to_share_between_request_workers() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();
    }

    #[test]
    fn newer_database_versions_are_never_downgraded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("future.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);
        assert!(matches!(
            Store::open(&path),
            Err(StoreError::UnsupportedSchema(5))
        ));
        let connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION + 1
        );
    }
}
