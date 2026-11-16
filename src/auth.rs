//! Account identities are safe to serialize. Passwords, password hashes, and
//! bearer session tokens never become client-visible application data.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub is_admin: bool,
}

#[cfg(feature = "ssr")]
pub use implementation::{Account, AuthError, AuthService, Credentials};
#[cfg(feature = "ssr")]
pub type AuthSession = axum_login::AuthSession<AuthService>;
#[cfg(feature = "ssr")]
pub use crate::session::session_layer;

#[cfg(feature = "ssr")]
mod implementation {
    use super::User;
    use crate::store::{Store, StoreError};
    use argon2::{
        Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
        password_hash::SaltString,
    };
    use axum_login::{AuthUser, AuthnBackend, UserId};
    use chrono::{DateTime, Utc};
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        error::Error,
        fmt,
        sync::{Arc, Mutex},
    };
    use tokio::sync::Semaphore;
    use uuid::Uuid;

    const HASH_CONCURRENCY: usize = 2;
    const RATE_WINDOW_SECONDS: i64 = 300;
    const MAX_RATE_BUCKETS: usize = 4096;
    const GLOBAL_LIMIT: u32 = 120;
    const CLIENT_LIMIT: u32 = 30;
    const USERNAME_LIMIT: u32 = 15;
    const PAIR_LIMIT: u32 = 8;

    /// Server-only account representation. Only the public identity crosses the
    /// Leptos boundary; the password hash also invalidates sessions after a
    /// future credential change through axum-login's session-auth-hash check.
    #[derive(Clone)]
    pub struct Account {
        pub identity: User,
        password_hash: String,
    }
    impl fmt::Debug for Account {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Account")
                .field("identity", &self.identity)
                .finish_non_exhaustive()
        }
    }
    impl AuthUser for Account {
        type Id = String;
        fn id(&self) -> Self::Id {
            self.identity.id.clone()
        }
        fn session_auth_hash(&self) -> &[u8] {
            self.password_hash.as_bytes()
        }
    }

    /// Credentials deliberately have no Debug/Serialize implementation.
    #[derive(Clone)]
    pub struct Credentials {
        pub username: String,
        pub password: String,
        pub client_key: String,
    }

    #[derive(Debug)]
    pub enum AuthError {
        InvalidInput(&'static str),
        InvalidCredentials,
        AccountUnavailable,
        RegistrationClosed,
        AdminRequired,
        RateLimited,
        Busy,
        Unavailable,
    }

    impl fmt::Display for AuthError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Self::InvalidInput(message) => message,
                Self::InvalidCredentials => "The username or password is incorrect.",
                Self::AccountUnavailable => "Unable to create this account. Try another username.",
                Self::RegistrationClosed => {
                    "Setup is complete. Ask an administrator to create your account."
                }
                Self::AdminRequired => "Administrator access is required.",
                Self::RateLimited => "Too many attempts. Please wait five minutes and try again.",
                Self::Busy => "Sign-in is busy. Please try again shortly.",
                Self::Unavailable => "Account access is temporarily unavailable. Please try again.",
            })
        }
    }
    impl Error for AuthError {}
    impl From<StoreError> for AuthError {
        fn from(error: StoreError) -> Self {
            match error {
                StoreError::AccountExists => Self::AccountUnavailable,
                StoreError::RegistrationClosed => Self::RegistrationClosed,
                StoreError::AdminRequired => Self::AdminRequired,
                _ => Self::Unavailable,
            }
        }
    }

    #[derive(Clone)]
    pub struct AuthService {
        store: Store,
        dummy_hash: Arc<String>,
        hash_slots: Arc<Semaphore>,
        limits: Arc<Mutex<RateLimiter>>,
    }

    impl AuthService {
        pub async fn new(store: Store) -> Result<Self, AuthError> {
            // Startup hashing also stays off the async executor. The random
            // unknown-user secret is never a real account credential.
            let dummy_hash = tokio::task::spawn_blocking(|| hash_password(&random_hex()?))
                .await
                .map_err(|_| AuthError::Unavailable)??;
            Ok(Self {
                store,
                dummy_hash: Arc::new(dummy_hash),
                hash_slots: Arc::new(Semaphore::new(HASH_CONCURRENCY)),
                limits: Arc::new(Mutex::new(RateLimiter::default())),
            })
        }

        pub async fn register(
            &self,
            username: String,
            password: String,
            client_key: String,
            now: DateTime<Utc>,
        ) -> Result<Account, AuthError> {
            self.create_account(None, username, password, client_key, now)
                .await
        }

        pub async fn create_user(
            &self,
            admin_id: String,
            username: String,
            password: String,
            client_key: String,
            now: DateTime<Utc>,
        ) -> Result<User, AuthError> {
            self.create_account(Some(admin_id), username, password, client_key, now)
                .await
                .map(|account| account.identity)
        }

        async fn create_account(
            &self,
            admin_id: Option<String>,
            username: String,
            password: String,
            client_key: String,
            now: DateTime<Utc>,
        ) -> Result<Account, AuthError> {
            self.check_rate(&client_key, &username, now)?;
            if admin_id.is_none() && !self.store.registration_open().await? {
                return Err(AuthError::RegistrationClosed);
            }
            let username = normalize_username(&username)?;
            validate_password(&password)?;
            let password_hash = self.run_hash(move || hash_password(&password)).await?;
            let user = User {
                id: Uuid::new_v4().to_string(),
                username,
                is_admin: false,
            };
            let user = self
                .store
                .create_account(user, password_hash.clone(), admin_id)
                .await?;
            Ok(Account {
                identity: user,
                password_hash,
            })
        }

        async fn verify_credentials(
            &self,
            credentials: Credentials,
        ) -> Result<Option<Account>, AuthError> {
            let Credentials {
                username,
                password,
                client_key,
            } = credentials;
            self.check_rate(&client_key, &username, Utc::now())?;
            let Ok(username) = normalize_username(&username) else {
                return Ok(None);
            };
            if validate_password(&password).is_err() {
                return Ok(None);
            }
            let account = self.store.credentials(username).await?;
            let stored_hash = account
                .as_ref()
                .map(|(_, hash)| hash.clone())
                .unwrap_or_else(|| self.dummy_hash.as_ref().clone());
            let valid = self
                .run_hash(move || {
                    let parsed =
                        PasswordHash::new(&stored_hash).map_err(|_| AuthError::Unavailable)?;
                    Ok(argon2()?
                        .verify_password(password.as_bytes(), &parsed)
                        .is_ok())
                })
                .await?;
            Ok(account
                .filter(|_| valid)
                .map(|(identity, password_hash)| Account {
                    identity,
                    password_hash,
                }))
        }

        fn check_rate(
            &self,
            client_key: &str,
            username: &str,
            now: DateTime<Utc>,
        ) -> Result<(), AuthError> {
            // Bound attacker-controlled key processing before allocation. The
            // socket-derived client key has no user-supplied forwarding headers.
            if client_key.len() > 128 || username.len() > 128 {
                return Err(AuthError::InvalidInput("Invalid account details."));
            }
            self.limits
                .lock()
                .map_err(|_| AuthError::Unavailable)?
                .check(
                    client_key,
                    &username.trim().to_ascii_lowercase(),
                    now.timestamp(),
                )
        }

        async fn run_hash<T, F>(&self, task: F) -> Result<T, AuthError>
        where
            T: Send + 'static,
            F: FnOnce() -> Result<T, AuthError> + Send + 'static,
        {
            // Do not queue unbounded login requests waiting for scarce Argon2
            // memory. A permit lives inside the blocking task even if its HTTP
            // future is cancelled, so cancellation cannot bypass the bound.
            let permit = Arc::clone(&self.hash_slots)
                .try_acquire_owned()
                .map_err(|_| AuthError::Busy)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                task()
            })
            .await
            .map_err(|_| AuthError::Unavailable)?
        }
    }

    impl AuthnBackend for AuthService {
        type User = Account;
        type Credentials = Credentials;
        type Error = AuthError;

        async fn authenticate(
            &self,
            credentials: Credentials,
        ) -> Result<Option<Account>, AuthError> {
            self.verify_credentials(credentials).await
        }
        async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Account>, AuthError> {
            Ok(self.store.credentials_by_id(user_id.clone()).await?.map(
                |(identity, password_hash)| Account {
                    identity,
                    password_hash,
                },
            ))
        }
    }

    fn normalize_username(username: &str) -> Result<String, AuthError> {
        let username = username.trim();
        if !(3..=32).contains(&username.len())
            || !username
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
        {
            return Err(AuthError::InvalidInput(
                "Use 3–32 letters, numbers, or underscores for your username.",
            ));
        }
        Ok(username.to_ascii_lowercase())
    }

    fn validate_password(password: &str) -> Result<(), AuthError> {
        if password.len() > 512 || !(15..=128).contains(&password.chars().count()) {
            return Err(AuthError::InvalidInput(
                "Use a password with 15–128 characters. Spaces and Unicode are welcome.",
            ));
        }
        Ok(())
    }

    fn argon2() -> Result<Argon2<'static>, AuthError> {
        // OWASP minimum Argon2id work factors: 19 MiB, 2 iterations, 1 lane.
        let parameters =
            Params::new(19 * 1024, 2, 1, Some(32)).map_err(|_| AuthError::Unavailable)?;
        Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters))
    }

    fn hash_password(password: &str) -> Result<String, AuthError> {
        let mut salt_bytes = [0_u8; 16];
        getrandom::fill(&mut salt_bytes).map_err(|_| AuthError::Unavailable)?;
        let salt = SaltString::encode_b64(&salt_bytes).map_err(|_| AuthError::Unavailable)?;
        argon2()?
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|_| AuthError::Unavailable)
    }

    fn random_hex() -> Result<String, AuthError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| AuthError::Unavailable)?;
        Ok(hex(&bytes))
    }

    fn hex(bytes: &[u8]) -> String {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(bytes.len() * 2);
        for &byte in bytes {
            output.push(DIGITS[(byte >> 4) as usize] as char);
            output.push(DIGITS[(byte & 15) as usize] as char);
        }
        output
    }

    #[derive(Default)]
    struct RateLimiter {
        buckets: HashMap<String, (i64, u32)>,
        global: Option<(i64, u32)>,
    }

    impl RateLimiter {
        fn check(&mut self, client: &str, username: &str, now: i64) -> Result<(), AuthError> {
            self.buckets
                .retain(|_, (start, _)| now < start.saturating_add(RATE_WINDOW_SECONDS));
            let global = self.global.get_or_insert((now, 0));
            if now >= global.0.saturating_add(RATE_WINDOW_SECONDS) {
                *global = (now, 0);
            }
            if global.1 >= GLOBAL_LIMIT {
                return Err(AuthError::RateLimited);
            }
            global.1 += 1;
            let client = hex(&Sha256::digest(client.as_bytes()));
            let username = hex(&Sha256::digest(username.as_bytes()));
            let keys = [
                (format!("i:{client}"), CLIENT_LIMIT),
                (format!("u:{username}"), USERNAME_LIMIT),
                (format!("p:{client}:{username}"), PAIR_LIMIT),
            ];
            let new_keys = keys
                .iter()
                .filter(|(key, _)| !self.buckets.contains_key(key))
                .count();
            if self.buckets.len() + new_keys > MAX_RATE_BUCKETS {
                return Err(AuthError::RateLimited);
            }
            // Increment each relevant bucket, including for rejected requests.
            // Changing usernames cannot bypass the per-client or global bound.
            let mut allowed = true;
            for (key, limit) in keys {
                let entry = self.buckets.entry(key).or_insert((now, 0));
                entry.1 = entry.1.saturating_add(1);
                if entry.1 > limit {
                    allowed = false;
                }
            }
            if allowed {
                Ok(())
            } else {
                Err(AuthError::RateLimited)
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use chrono::TimeZone;
        use tempfile::TempDir;

        const PASSWORD: &str = "A long test passphrase only";
        fn now() -> DateTime<Utc> {
            Utc.with_ymd_and_hms(2026, 10, 21, 12, 0, 0).unwrap()
        }
        async fn service() -> (TempDir, AuthService) {
            let directory = tempfile::tempdir().unwrap();
            let store = Store::open(&directory.path().join("accounts.sqlite3")).unwrap();
            (directory, AuthService::new(store).await.unwrap())
        }

        #[test]
        fn input_rules_are_unicode_friendly_bounded_and_canonical() {
            assert_eq!(normalize_username(" Alice_01 ").unwrap(), "alice_01");
            for name in ["ab", "a b", "ééé", "alice@example.com", "<script>"] {
                assert!(normalize_username(name).is_err());
            }
            assert!(normalize_username(&"a".repeat(33)).is_err());
            assert!(validate_password(&"a".repeat(14)).is_err());
            assert!(validate_password(&"a".repeat(15)).is_ok());
            assert!(validate_password(&"🦊".repeat(128)).is_ok());
            assert!(validate_password(&"🦊".repeat(129)).is_err());
            assert!(validate_password(" spaces are not trimmed ").is_ok());
        }

        #[tokio::test]
        async fn credentials_are_salted_persistent_and_generic_on_failure() {
            let (directory, auth) = service().await;
            let account = auth
                .register(" Alice ".into(), PASSWORD.into(), "192.0.2.1".into(), now())
                .await
                .unwrap();
            assert_eq!(account.identity.username, "alice");
            assert!(
                account
                    .password_hash
                    .starts_with("$argon2id$v=19$m=19456,t=2,p=1$")
            );
            assert!(!format!("{account:?}").contains(&account.password_hash));
            for (username, password) in [("alice", "Wrong test passphrase"), ("unknown", PASSWORD)]
            {
                assert!(
                    auth.authenticate(Credentials {
                        username: username.into(),
                        password: password.into(),
                        client_key: "192.0.2.2".into()
                    })
                    .await
                    .unwrap()
                    .is_none()
                );
            }
            let reopened =
                AuthService::new(Store::open(&directory.path().join("accounts.sqlite3")).unwrap())
                    .await
                    .unwrap();
            let login = reopened
                .authenticate(Credentials {
                    username: "ALICE".into(),
                    password: PASSWORD.into(),
                    client_key: "192.0.2.3".into(),
                })
                .await
                .unwrap()
                .unwrap();
            assert_eq!(login.identity, account.identity);
            assert_eq!(
                reopened
                    .get_user(&account.identity.id)
                    .await
                    .unwrap()
                    .unwrap()
                    .identity,
                account.identity
            );
            assert!(
                reopened
                    .get_user(&uuid::Uuid::new_v4().to_string())
                    .await
                    .unwrap()
                    .is_none()
            );
        }

        #[tokio::test]
        async fn only_one_concurrent_signup_becomes_an_administrator() {
            let (_directory, auth) = service().await;
            let (first, second) = tokio::join!(
                auth.register("alice".into(), PASSWORD.into(), "192.0.2.1".into(), now()),
                auth.register("bravo".into(), PASSWORD.into(), "192.0.2.2".into(), now())
            );
            let successful = match (first, second) {
                (Ok(a), Err(AuthError::RegistrationClosed))
                | (Err(AuthError::RegistrationClosed), Ok(a)) => a,
                results => panic!("Expected one administrator and one closed signup: {results:?}"),
            };
            assert!(successful.identity.is_admin);
            assert!(!auth.store.registration_open().await.unwrap());
            assert_eq!(
                auth.store
                    .users_for_admin(successful.identity.id)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        }

        #[tokio::test]
        async fn admin_creation_is_case_insensitive_and_salts_each_password() {
            let (_directory, auth) = service().await;
            let admin = auth
                .register("owner".into(), PASSWORD.into(), "192.0.2.1".into(), now())
                .await
                .unwrap();
            let (first, second) = tokio::join!(
                auth.create_user(
                    admin.identity.id.clone(),
                    "alice".into(),
                    PASSWORD.into(),
                    "192.0.2.2".into(),
                    now()
                ),
                auth.create_user(
                    admin.identity.id.clone(),
                    "ALICE".into(),
                    PASSWORD.into(),
                    "192.0.2.3".into(),
                    now()
                ),
            );
            let successful = match (first, second) {
                (Ok(user), Err(AuthError::AccountUnavailable))
                | (Err(AuthError::AccountUnavailable), Ok(user)) => user,
                results => panic!("Expected one member and one duplicate rejection: {results:?}"),
            };
            assert!(!successful.is_admin);
            let other = auth
                .create_user(
                    admin.identity.id,
                    "bravo".into(),
                    PASSWORD.into(),
                    "192.0.2.4".into(),
                    now(),
                )
                .await
                .unwrap();
            let first_hash = auth
                .store
                .credentials_by_id(successful.id.clone())
                .await
                .unwrap()
                .unwrap()
                .1;
            let second_hash = auth
                .store
                .credentials_by_id(other.id)
                .await
                .unwrap()
                .unwrap()
                .1;
            assert_ne!(first_hash, second_hash);
            assert!(
                auth.store
                    .snapshot(successful.id)
                    .await
                    .unwrap()
                    .systems
                    .is_empty()
            );
        }

        #[test]
        fn rate_limits_cover_pair_username_client_global_and_expire() {
            let mut pair = RateLimiter::default();
            for _ in 0..PAIR_LIMIT {
                pair.check("one", "alice", 100).unwrap();
            }
            assert!(matches!(
                pair.check("one", "alice", 100),
                Err(AuthError::RateLimited)
            ));
            pair.check("one", "alice", 100 + RATE_WINDOW_SECONDS)
                .unwrap();
            let mut username = RateLimiter::default();
            for i in 0..USERNAME_LIMIT {
                username.check(&format!("client{i}"), "alice", 100).unwrap();
            }
            assert!(username.check("different-client", "alice", 100).is_err());
            let mut client = RateLimiter::default();
            for i in 0..CLIENT_LIMIT {
                client.check("one", &format!("user{i}"), 100).unwrap();
            }
            assert!(client.check("one", "new-user", 100).is_err());
            let mut global = RateLimiter::default();
            for i in 0..GLOBAL_LIMIT {
                global
                    .check(&format!("client{i}"), &format!("user{i}"), 100)
                    .unwrap();
            }
            assert!(global.check("new-client", "new-user", 100).is_err());
            assert!(global.buckets.len() <= MAX_RATE_BUCKETS);
            global
                .check("new-client", "new-user", 100 + RATE_WINDOW_SECONDS)
                .unwrap();
            assert_eq!(global.buckets.len(), 3);
        }

        #[test]
        fn rate_limiter_fails_closed_at_memory_bound() {
            let mut limits = RateLimiter::default();
            for i in 0..MAX_RATE_BUCKETS {
                limits.buckets.insert(format!("seed-{i}"), (100, 1));
            }
            assert!(limits.check("client", "alice", 100).is_err());
            assert_eq!(limits.buckets.len(), MAX_RATE_BUCKETS);
            limits
                .check("client", "alice", 100 + RATE_WINDOW_SECONDS)
                .unwrap();
            assert_eq!(limits.buckets.len(), 3);
        }

        #[tokio::test]
        async fn hash_capacity_rejects_instead_of_building_an_unbounded_queue() {
            let (_directory, auth) = service().await;
            let _all_slots = Arc::clone(&auth.hash_slots)
                .acquire_many_owned(HASH_CONCURRENCY as u32)
                .await
                .unwrap();
            assert!(matches!(
                auth.run_hash(|| Ok(())).await,
                Err(AuthError::Busy)
            ));
        }
    }
}
