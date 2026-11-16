//! The application's network boundary. Server functions return feature-shaped
//! data, never database rows or SQL. Both rendered HTML and hydrated Rust use
//! this same API; the SQLite implementation is absent from the WASM build.
use crate::{auth::User, domain::*};
use chrono::NaiveDate;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodaySystem {
    pub system: System,
    pub momentum: Momentum,
    pub today_state: DayState,
    pub day: NaiveDate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TodayData {
    pub today: NaiveDate,
    pub time_zone: String,
    pub systems: Vec<TodaySystem>,
    pub archived: Vec<System>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EditorData {
    /// A stable creation key makes resubmission an update, not a duplicate.
    pub id: String,
    pub system: Option<System>,
    pub time_zone: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewData {
    pub review: WeeklyReview,
    pub reflection: Reflection,
}

#[cfg(feature = "ssr")]
#[derive(Clone)]
pub struct AppContext {
    pub store: crate::store::Store,
    pub time_zone: String,
    pub auth: crate::auth::AuthService,
}

#[cfg(feature = "ssr")]
fn context() -> Result<AppContext, ServerFnError> {
    use_context::<AppContext>()
        .ok_or_else(|| ServerFnError::new("The app is not ready. Please try again."))
}

/// Storage internals are logged on the server, not exposed to the browser.
#[cfg(feature = "ssr")]
fn storage_error(error: crate::store::StoreError) -> ServerFnError {
    use crate::store::StoreError;
    match error {
        StoreError::InvalidInput(message) => ServerFnError::new(message),
        StoreError::NotFound => {
            ServerFnError::new("That system could not be found. Return to Today and try again.")
        }
        StoreError::Archived => ServerFnError::new("Restore this system before logging a day."),
        other => {
            eprintln!("Storage operation failed: {other}");
            ServerFnError::new(
                "Your changes could not be saved. Please try again; your form is still here.",
            )
        }
    }
}

#[server]
pub async fn get_today(expected_user_id: String) -> Result<TodayData, ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    require_same_account(&user, &expected_user_id)?;
    let today = today_in_timezone(&ctx.time_zone)?;
    let snapshot = ctx.store.snapshot(user.id).await.map_err(storage_error)?;
    let mut systems = Vec::new();
    let mut archived = Vec::new();
    for system in snapshot.systems {
        if system.archived {
            archived.push(system);
        } else {
            let system_today = today_in_timezone(&system.time_zone)?;
            let momentum = compute_momentum(&system, &snapshot.entries, system_today, None)?;
            let today_state = momentum
                .series
                .last()
                .map(|day| day.state)
                .unwrap_or(DayState::Miss);
            systems.push(TodaySystem {
                system,
                momentum,
                today_state,
                day: system_today,
            });
        }
    }
    Ok(TodayData {
        today,
        time_zone: ctx.time_zone,
        systems,
        archived,
    })
}

#[server]
pub async fn get_editor(
    system_id: Option<String>,
    expected_user_id: String,
) -> Result<EditorData, ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    require_same_account(&user, &expected_user_id)?;
    let system = if let Some(id) = system_id {
        ctx.store
            .snapshot(user.id)
            .await
            .map_err(storage_error)?
            .systems
            .into_iter()
            .find(|system| system.id == id)
            .ok_or_else(|| ServerFnError::new("That system could not be found."))?
            .into()
    } else {
        None
    };
    let id = system
        .as_ref()
        .map(|system: &System| system.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    Ok(EditorData {
        id,
        system,
        time_zone: ctx.time_zone,
    })
}

#[server]
pub async fn save_system(
    id: String,
    draft: SystemDraft,
    expected_user_id: String,
) -> Result<(), ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    require_same_account(&user, &expected_user_id)?;
    ctx.store
        .save_system(user.id, id, draft, ctx.time_zone)
        .await
        .map_err(storage_error)
}

/// The client sends the desired state, never a toggle command. Retrying a
/// request cannot accidentally undo it, and midnight is resolved by the server.
#[server]
pub async fn set_check_in(
    system_id: String,
    tier: Option<Tier>,
    expected_day: NaiveDate,
) -> Result<(), ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    ctx.store
        .check_in(user.id, system_id, tier, expected_day, chrono::Utc::now())
        .await
        .map_err(storage_error)
}

#[server]
pub async fn set_archived(system_id: String, archived: bool) -> Result<(), ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    ctx.store
        .archive_system(user.id, system_id, archived)
        .await
        .map_err(storage_error)
}

#[server]
pub async fn get_weekly_review(expected_user_id: String) -> Result<ReviewData, ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    require_same_account(&user, &expected_user_id)?;
    let today = today_in_timezone(&ctx.time_zone)?;
    let snapshot = ctx.store.snapshot(user.id).await.map_err(storage_error)?;
    let review = build_weekly_review(&snapshot.systems, &snapshot.entries, today)?;
    let reflection = snapshot
        .reflections
        .into_iter()
        .find(|r| r.week_start == review.week_start)
        .unwrap_or_else(|| Reflection::empty(review.week_start));
    Ok(ReviewData { review, reflection })
}

#[server]
pub async fn save_reflection(
    reflection: Reflection,
    expected_user_id: String,
) -> Result<(), ServerFnError> {
    let ctx = context()?;
    let user = require_user(&ctx).await?;
    require_same_account(&user, &expected_user_id)?;
    ctx.store
        .save_reflection(user.id, reflection)
        .await
        .map_err(storage_error)
}

#[cfg(feature = "ssr")]
fn request_parts() -> Result<axum::http::request::Parts, ServerFnError> {
    use_context::<axum::http::request::Parts>()
        .ok_or_else(|| ServerFnError::new("The request could not be verified. Please try again."))
}

#[cfg(feature = "ssr")]
fn auth_error(error: crate::auth::AuthError) -> ServerFnError {
    // AuthError's public Display contract must never contain SQL, passwords,
    // session tokens or account-existence details.
    if let Some(response) = use_context::<leptos_axum::ResponseOptions>() {
        use crate::auth::AuthError;
        use axum::http::{StatusCode, header};
        response.set_status(match &error {
            AuthError::InvalidCredentials => StatusCode::UNAUTHORIZED,
            AuthError::RegistrationClosed | AuthError::AdminRequired => StatusCode::FORBIDDEN,
            AuthError::RateLimited => {
                response.insert_header(header::RETRY_AFTER, "300".parse().expect("fixed header"));
                StatusCode::TOO_MANY_REQUESTS
            }
            AuthError::Busy | AuthError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            AuthError::InvalidInput(_) | AuthError::AccountUnavailable => StatusCode::BAD_REQUEST,
        });
    }
    ServerFnError::new(error.to_string())
}

/// Extract the library-authenticated session in both SSR and server functions.
/// UI guards are convenience only; every private endpoint calls require_user.
#[cfg(feature = "ssr")]
async fn auth_session() -> Result<crate::auth::AuthSession, ServerFnError> {
    leptos_axum::extract::<crate::auth::AuthSession>()
        .await
        .map_err(|_| auth_error(crate::auth::AuthError::Unavailable))
}

#[cfg(feature = "ssr")]
async fn require_user(_ctx: &AppContext) -> Result<User, ServerFnError> {
    auth_session()
        .await?
        .user
        .map(|account| account.identity)
        .ok_or_else(|| {
            if let Some(response) = use_context::<leptos_axum::ResponseOptions>() {
                response.set_status(axum::http::StatusCode::UNAUTHORIZED);
            }
            ServerFnError::new("Please sign in to continue.")
        })
}

#[cfg(feature = "ssr")]
async fn require_admin(ctx: &AppContext, expected_user_id: &str) -> Result<User, ServerFnError> {
    let user = require_user(ctx).await?;
    require_same_account(&user, expected_user_id)?;
    if !user.is_admin {
        return Err(auth_error(crate::auth::AuthError::AdminRequired));
    }
    Ok(user)
}

#[cfg(feature = "ssr")]
fn session_error(error: axum_login::Error<crate::auth::AuthService>) -> ServerFnError {
    match error {
        axum_login::Error::Backend(error) => auth_error(error),
        axum_login::Error::Session(_) => auth_error(crate::auth::AuthError::Unavailable),
    }
}

/// Rotate even when an already-authenticated browser signs in again. The
/// library's login cycles anonymous IDs; explicitly cycling closes the same-user
/// and cross-account cases too. No habit data is stored in the session.
#[cfg(feature = "ssr")]
async fn establish_session(
    session: &mut crate::auth::AuthSession,
    account: &crate::auth::Account,
) -> Result<(), ServerFnError> {
    session
        .session
        .cycle_id()
        .await
        .map_err(|_| auth_error(crate::auth::AuthError::Unavailable))?;
    session
        .session
        .set_expiry(Some(tower_sessions::Expiry::AtDateTime(
            time::OffsetDateTime::now_utc() + time::Duration::days(7),
        )));
    session.login(account).await.map_err(session_error)
}

/// Prevent a stale tab's draft from being written into a different account
/// after another tab changed the shared session cookie. This is an additional
/// precondition, never a client-selected authorization identity.
#[cfg(feature = "ssr")]
fn require_same_account(user: &User, expected_user_id: &str) -> Result<(), ServerFnError> {
    if user.id == expected_user_id {
        Ok(())
    } else {
        if let Some(response) = use_context::<leptos_axum::ResponseOptions>() {
            response.set_status(axum::http::StatusCode::UNAUTHORIZED);
        }
        Err(ServerFnError::new("Please sign in to continue."))
    }
}

#[server(endpoint = "current_user")]
pub async fn current_user() -> Result<Option<User>, ServerFnError> {
    Ok(auth_session().await?.user.map(|account| account.identity))
}

#[server(endpoint = "login")]
pub async fn login(username: String, password: String) -> Result<User, ServerFnError> {
    let parts = request_parts()?;
    let mut session = auth_session().await?;
    let account = session
        .authenticate(crate::auth::Credentials {
            username,
            password,
            client_key: crate::security::peer_key(&parts),
        })
        .await
        .map_err(session_error)?
        .ok_or_else(|| auth_error(crate::auth::AuthError::InvalidCredentials))?;
    establish_session(&mut session, &account).await?;
    Ok(account.identity)
}

#[server(endpoint = "register")]
pub async fn register(username: String, password: String) -> Result<User, ServerFnError> {
    let ctx = context()?;
    let parts = request_parts()?;
    let mut session = auth_session().await?;
    let account = ctx
        .auth
        .register(
            username,
            password,
            crate::security::peer_key(&parts),
            chrono::Utc::now(),
        )
        .await
        .map_err(auth_error)?;
    // Account creation remains successful if storage fails while starting the
    // session: direct the user to Sign in rather than repeat registration.
    establish_session(&mut session, &account).await.map_err(|_| {
        ServerFnError::new("Your account was created, but sign-in could not complete. Try signing in with your new account.")
    })?;
    Ok(account.identity)
}

#[server(endpoint = "registration_open")]
pub async fn registration_open() -> Result<bool, ServerFnError> {
    context()?
        .store
        .registration_open()
        .await
        .map_err(storage_error)
}

#[server(endpoint = "list_accounts")]
pub async fn list_accounts(expected_user_id: String) -> Result<Vec<User>, ServerFnError> {
    let ctx = context()?;
    let admin = require_admin(&ctx, &expected_user_id).await?;
    ctx.store
        .users_for_admin(admin.id)
        .await
        .map_err(|error| auth_error(error.into()))
}

#[server(endpoint = "create_account")]
pub async fn create_account(
    expected_user_id: String,
    username: String,
    password: String,
) -> Result<User, ServerFnError> {
    let ctx = context()?;
    let admin = require_admin(&ctx, &expected_user_id).await?;
    let parts = request_parts()?;
    // Creating an account must not replace or rotate the administrator's session.
    ctx.auth
        .create_user(
            admin.id,
            username,
            password,
            crate::security::peer_key(&parts),
            chrono::Utc::now(),
        )
        .await
        .map_err(auth_error)
}

#[server(endpoint = "logout")]
pub async fn logout() -> Result<(), ServerFnError> {
    auth_session()
        .await?
        .logout()
        .await
        .map_err(session_error)?;
    Ok(())
}
