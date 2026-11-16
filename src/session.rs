//! Library-managed session persistence and cookie policy. Habit storage never
//! reads these records; axum-login is the sole source of authenticated identity.
use crate::{auth::AuthError, security::SecurityConfig};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::{path::Path, time::Duration};
use tower_sessions::{ExpiredDeletion, SessionManagerLayer, cookie::SameSite};
use tower_sessions_sqlx_store::SqliteStore;

/// Open the library's independent table in the same SQLite file. The library
/// owns schema/serialization, ID generation, expiry lookup and deletion. Its
/// opaque IDs are bearer secrets at rest, so the database and backups are private.
/// Do not add application session writes: only login sets the fixed deadline,
/// and all later requests are read-only. Future session mutations must retain
/// the original absolute expiry rather than inherit the library default.
pub async fn session_layer(
    database: &Path,
    security: &SecurityConfig,
) -> Result<SessionManagerLayer<SqliteStore>, AuthError> {
    let options = SqliteConnectOptions::new()
        .filename(database)
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .map_err(|_| AuthError::Unavailable)?;
    let store = SqliteStore::new(pool);
    store.migrate().await.map_err(|_| AuthError::Unavailable)?;
    store
        .delete_expired()
        .await
        .map_err(|_| AuthError::Unavailable)?;
    // The task lives with this single-process runtime, and is cancelled at
    // shutdown. Expiry is enforced on reads even if cleanup later fails.
    let cleanup = store.clone();
    tokio::spawn(async move {
        if cleanup
            .continuously_delete_expired(Duration::from_secs(300))
            .await
            .is_err()
        {
            eprintln!("Expired-session cleanup stopped; restart the app after checking storage.");
        }
    });
    Ok(SessionManagerLayer::new(store)
        .with_name(security.cookie_name())
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_secure(security.secure_cookies)
        .with_path("/")
        .with_always_save(false))
}
