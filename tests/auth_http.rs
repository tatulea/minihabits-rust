#![cfg(feature = "ssr")]
#![recursion_limit = "256"]
//! Exercise the real Leptos server-function HTTP boundary, not just helper APIs.
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    middleware,
    response::Response,
};
use axum_login::AuthManagerLayerBuilder;
use leptos::{prelude::*, server_fn::ServerFn};
use leptos_axum::{LeptosRoutes, generate_route_list};
use minihabits_rust::{
    auth::{AuthService, User, session_layer},
    domain::{Reflection, SystemDraft},
    security::{SecurityConfig, request_guard},
    server::{
        AppContext, GetEditor, GetToday, GetWeeklyReview, SaveReflection, SaveSystem, SetArchived,
        SetCheckIn,
    },
    store::Store,
};
use std::{path::Path, sync::OnceLock, time::Duration};
use tower::ServiceExt;

// Route discovery globally suppresses resource loading in Leptos. Run it once,
// as at server startup, before any parallel test can start rendering a page.
static ROUTES: OnceLock<Vec<leptos_axum::AxumRouteListing>> = OnceLock::new();

async fn test_app() -> (tempfile::TempDir, Store, Router) {
    let directory = tempfile::tempdir().unwrap();
    let (store, app) = app_at(&directory.path().join("test.sqlite3")).await;
    (directory, store, app)
}

/// Rebuild every middleware/backend object over the same temporary database,
/// so persistence tests cannot accidentally reuse an in-memory auth session.
async fn app_at(database: &Path) -> (Store, Router) {
    app_at_origin(database, "https://habits.test").await
}

async fn app_at_origin(database: &Path, origin: &str) -> (Store, Router) {
    let store = Store::open(database).unwrap();
    let security = SecurityConfig::new(origin).unwrap();
    let ctx = AppContext {
        store: store.clone(),
        auth: AuthService::new(store.clone()).await.unwrap(),
        time_zone: "UTC".into(),
    };
    let sessions = session_layer(database, &security).await.unwrap();
    let auth_layer = AuthManagerLayerBuilder::new(ctx.auth.clone(), sessions).build();
    let options = LeptosOptions::builder()
        .output_name("minihabits-rust")
        .build();
    let routes = ROUTES
        .get_or_init(|| generate_route_list(minihabits_rust::app::App))
        .clone();
    let app = Router::new()
        .leptos_routes_with_context(&options, routes, move || provide_context(ctx.clone()), {
            let options = options.clone();
            move || minihabits_rust::app::shell(options.clone())
        })
        .layer(auth_layer)
        .layer(middleware::from_fn_with_state(security, request_guard))
        .with_state(options);
    (store, app)
}
async fn call(app: &Router, path: &str, body: &str, cookie: Option<&str>) -> Response {
    call_at_origin(app, "https://habits.test", path, body, cookie).await
}

async fn call_at_origin(
    app: &Router,
    origin: &str,
    path: &str,
    body: &str,
    cookie: Option<&str>,
) -> Response {
    let origin_uri: axum::http::Uri = origin.parse().unwrap();
    let mut request = Request::builder()
        .method("POST")
        .uri(path)
        .header(header::HOST, origin_uri.authority().unwrap().as_str())
        .header(header::ORIGIN, origin)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    app.clone()
        .oneshot(request.body(Body::from(body.to_owned())).unwrap())
        .await
        .unwrap()
}
async fn body(response: Response) -> String {
    String::from_utf8(
        tokio::time::timeout(
            Duration::from_secs(10),
            to_bytes(response.into_body(), 128 * 1024),
        )
        .await
        .expect("response body did not finish within 10 seconds")
        .unwrap()
        .to_vec(),
    )
    .unwrap()
}

async fn page(app: &Router, path: &str, cookie: Option<&str>) -> Response {
    let mut request = Request::builder()
        .uri(path)
        .header(header::ACCEPT, "text/html")
        .header(header::HOST, "habits.test");
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    tokio::time::timeout(
        Duration::from_secs(10),
        app.clone().oneshot(request.body(Body::empty()).unwrap()),
    )
    .await
    .unwrap_or_else(|_| panic!("page {path} did not respond within 10 seconds"))
    .unwrap()
}

#[tokio::test]
async fn entry_pages_show_only_setup_until_an_account_exists_then_only_sign_in() {
    let (_directory, _store, app) = test_app().await;
    let paths = [
        "/",
        "/app",
        "/login",
        "/register",
        "/app/design",
        "/app/accounts",
    ];
    for path in paths {
        let response = page(&app, path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let html = body(response).await;
        assert_eq!(html.matches("<form ").count(), 1, "{path}: {html}");
        assert!(html.contains("action=\"/api/register\""), "{path}: {html}");
        assert!(html.contains("Set up MiniHabits"));
        assert!(!html.contains("action=\"/api/login\""));
        assert!(!html.contains("href=\"/login\""));
        assert!(!html.contains("href=\"/register\""));
        assert!(!html.contains("aria-label=\"Primary\""));
    }

    let (_admin, cookie) = register(&app, "owner").await;
    for path in paths {
        let response = page(&app, path, None).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let html = body(response).await;
        assert_eq!(html.matches("<form ").count(), 1, "{path}: {html}");
        assert!(html.contains("action=\"/api/login\""), "{path}: {html}");
        assert!(!html.contains("action=\"/api/register\""));
        assert!(!html.contains("Set up MiniHabits"));
        assert!(!html.contains("href=\"/register\""));
        assert!(!html.contains("aria-label=\"Primary\""));
    }
    for path in ["/login", "/register"] {
        let response = page(&app, path, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(response.headers()[header::LOCATION], "/app");
    }
    let response = page(&app, "/app", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body(response).await;
    assert!(html.contains("aria-label=\"Primary\""));
    assert!(html.contains("href=\"/app/accounts\""));
    assert!(!html.contains("action=\"/api/login\""));
    assert!(!html.contains("action=\"/api/register\""));
    for (path, content) in [
        ("/app/design", "aria-label=\"System builder\""),
        ("/app/review", "aria-label=\"Weekly reflection\""),
        ("/app/accounts", "Existing accounts"),
    ] {
        let response = page(&app, path, Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let html = body(response).await;
        assert!(html.contains(content), "{path}: {html}");
        assert!(!html.contains("action=\"/api/login\""));
        assert!(!html.contains("action=\"/api/register\""));
    }
}

fn session_cookie_header(response: &Response) -> &str {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with("__Host-minihabits_session="))
        .expect("response must set the application session cookie")
}

fn session_cookie(response: &Response) -> String {
    session_cookie_header(response)
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn assert_hardened_session_cookie(response: &Response) {
    let cookie = tower_sessions::cookie::Cookie::parse(session_cookie_header(response)).unwrap();
    assert_eq!(cookie.secure(), Some(true));
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(
        cookie.same_site(),
        Some(tower_sessions::cookie::SameSite::Lax)
    );
    assert_eq!(cookie.path(), Some("/"));
    assert!(cookie.domain().is_none());
}

fn assert_session_cookie_removed(response: &Response) {
    // Browsers reject a __Host- deletion cookie unless Secure and Path=/ remain
    // present. Check the complete policy on expiry and auth-hash invalidation,
    // not just the explicit logout response.
    assert_hardened_session_cookie(response);
    let cookie = tower_sessions::cookie::Cookie::parse(session_cookie_header(response)).unwrap();
    assert!(cookie.value().is_empty());
    assert_eq!(cookie.max_age(), Some(time::Duration::ZERO));
}

async fn today_status(app: &Router, user: &User, cookie: &str) -> StatusCode {
    call(
        app,
        GetToday::PATH,
        &format!("expected_user_id={}", user.id),
        Some(cookie),
    )
    .await
    .status()
}

async fn register(app: &Router, username: &str) -> (User, String) {
    let response = call(
        app,
        "/api/register",
        &format!("username={username}&password=synthetic-test-password-2026"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_hardened_session_cookie(&response);
    assert!(session_cookie_header(&response).contains("Max-Age="));
    let cookie = session_cookie(&response);
    let response_body = body(response).await;
    assert!(!response_body.contains("synthetic-test-password"));
    assert!(!response_body.contains("argon2"));
    let user = serde_json::from_str(&response_body).unwrap();
    (user, cookie)
}

async fn add_member(
    app: &Router,
    admin: &User,
    admin_cookie: &str,
    username: &str,
) -> (User, String) {
    let response = call(
        app,
        "/api/create_account",
        &format!(
            "expected_user_id={}&username={username}&password=synthetic-test-password-2026",
            admin.id
        ),
        Some(admin_cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    let user: User = serde_json::from_str(&body(response).await).unwrap();
    assert!(!user.is_admin);
    let response = call(
        app,
        "/api/login",
        &format!("username={username}&password=synthetic-test-password-2026"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_hardened_session_cookie(&response);
    let cookie = session_cookie(&response);
    assert_eq!(
        serde_json::from_str::<User>(&body(response).await).unwrap(),
        user
    );
    (user, cookie)
}

#[tokio::test]
async fn setup_closes_registration_and_admin_can_create_accounts_without_switching_sessions() {
    let (directory, _store, app) = test_app().await;
    assert_eq!(
        body(call(&app, "/api/registration_open", "", None).await).await,
        "true"
    );
    let invalid = call(&app, "/api/register", "username=alice&password=short", None).await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body(call(&app, "/api/registration_open", "", None).await).await,
        "true"
    );

    let (admin, admin_cookie) = register(&app, "alice").await;
    assert!(admin.is_admin);
    assert_eq!(
        body(call(&app, "/api/registration_open", "", None).await).await,
        "false"
    );
    for cookie in [None, Some(admin_cookie.as_str())] {
        let rejected = call(
            &app,
            "/api/register",
            "username=intruder&password=synthetic-test-password-2026",
            cookie,
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        assert!(!rejected.headers().contains_key(header::SET_COOKIE));
    }

    let (member, member_cookie) = add_member(&app, &admin, &admin_cookie, "bob").await;
    assert!(!member.is_admin);
    let current = call(&app, "/api/current_user", "", Some(&admin_cookie)).await;
    assert_eq!(
        serde_json::from_str::<Option<User>>(&body(current).await).unwrap(),
        Some(admin.clone())
    );
    assert_eq!(
        today_status(&app, &member, &member_cookie).await,
        StatusCode::OK
    );
    let listed = call(
        &app,
        "/api/list_accounts",
        &format!("expected_user_id={}", admin.id),
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body(listed).await;
    assert!(!listed.contains("argon2") && !listed.contains("password"));
    assert_eq!(
        serde_json::from_str::<Vec<User>>(&listed).unwrap(),
        vec![admin.clone(), member]
    );

    let (_store, restarted) = app_at(&directory.path().join("test.sqlite3")).await;
    assert_eq!(
        body(call(&restarted, "/api/registration_open", "", None).await).await,
        "false"
    );
    let rejected = call(
        &restarted,
        "/api/register",
        "username=another&password=synthetic-test-password-2026",
        None,
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let current = call(&restarted, "/api/current_user", "", Some(&admin_cookie)).await;
    assert_eq!(
        serde_json::from_str::<Option<User>>(&body(current).await).unwrap(),
        Some(admin)
    );
}

#[tokio::test]
async fn account_management_rejects_guests_members_stale_tabs_and_role_injection() {
    let (_directory, _store, app) = test_app().await;
    let (admin, admin_cookie) = register(&app, "alice").await;
    let (member, member_cookie) = add_member(&app, &admin, &admin_cookie, "bob").await;
    for (cookie, expected_user_id, expected_status) in [
        (None, admin.id.as_str(), StatusCode::UNAUTHORIZED),
        (
            Some(member_cookie.as_str()),
            member.id.as_str(),
            StatusCode::FORBIDDEN,
        ),
        (
            Some(member_cookie.as_str()),
            admin.id.as_str(),
            StatusCode::UNAUTHORIZED,
        ),
        (
            Some(admin_cookie.as_str()),
            member.id.as_str(),
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        for endpoint in ["/api/list_accounts", "/api/create_account"] {
            let request_body = format!(
                "expected_user_id={expected_user_id}&username=blocked&password=synthetic-test-password-2026&is_admin=true"
            );
            let rejected = call(&app, endpoint, &request_body, cookie).await;
            assert_eq!(rejected.status(), expected_status);
        }
    }
    for credentials in [
        "username=BOB&password=synthetic-test-password-2026",
        "username=invalid&password=short",
    ] {
        let rejected = call(
            &app,
            "/api/create_account",
            &format!("expected_user_id={}&{credentials}", admin.id),
            Some(&admin_cookie),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    }
    let created = call(&app, "/api/create_account", &format!("expected_user_id={}&username=carol&password=synthetic-test-password-2026&is_admin=true", admin.id), Some(&admin_cookie)).await;
    assert_eq!(created.status(), StatusCode::OK);
    assert!(!created.headers().contains_key(header::SET_COOKIE));
    assert!(
        !serde_json::from_str::<User>(&body(created).await)
            .unwrap()
            .is_admin
    );
    let listed = call(
        &app,
        "/api/list_accounts",
        &format!("expected_user_id={}", admin.id),
        Some(&admin_cookie),
    )
    .await;
    let users: Vec<User> = serde_json::from_str(&body(listed).await).unwrap();
    assert_eq!(
        users
            .iter()
            .map(|user| user.username.as_str())
            .collect::<Vec<_>>(),
        vec!["alice", "bob", "carol"]
    );
    assert_eq!(users.iter().filter(|user| user.is_admin).count(), 1);
}

#[tokio::test]
async fn simultaneous_public_signups_create_only_one_admin() {
    let (_directory, _store, app) = test_app().await;
    let (first, second) = tokio::join!(
        call(
            &app,
            "/api/register",
            "username=alice&password=synthetic-test-password-2026",
            None
        ),
        call(
            &app,
            "/api/register",
            "username=bravo&password=synthetic-test-password-2026",
            None
        ),
    );
    let (winner, loser) = if first.status() == StatusCode::OK {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(winner.status(), StatusCode::OK);
    assert_eq!(loser.status(), StatusCode::FORBIDDEN);
    assert!(!loser.headers().contains_key(header::SET_COOKIE));
    let cookie = session_cookie(&winner);
    let admin: User = serde_json::from_str(&body(winner).await).unwrap();
    assert!(admin.is_admin);
    let listed = call(
        &app,
        "/api/list_accounts",
        &format!("expected_user_id={}", admin.id),
        Some(&cookie),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Vec<User>>(&body(listed).await).unwrap(),
        vec![admin]
    );
}

#[tokio::test]
async fn local_http_sessions_work_without_secure_cookies() {
    let directory = tempfile::tempdir().unwrap();
    let origin = "http://localhost:5175";
    let (_store, app) = app_at_origin(&directory.path().join("test.sqlite3"), origin).await;
    let registered = call_at_origin(
        &app,
        origin,
        "/api/register",
        "username=local_user&password=synthetic-test-password-2026",
        None,
    )
    .await;
    assert_eq!(registered.status(), StatusCode::OK);
    let header = registered.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = tower_sessions::cookie::Cookie::parse(header).unwrap();
    assert_eq!(cookie.name(), "minihabits_session");
    assert!(!cookie.secure().unwrap_or(false));
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(
        cookie.same_site(),
        Some(tower_sessions::cookie::SameSite::Lax)
    );
    assert_eq!(cookie.path(), Some("/"));
    assert!(cookie.domain().is_none());
    let session = format!("{}={}", cookie.name(), cookie.value());
    let user: User = serde_json::from_str(&body(registered).await).unwrap();
    let request_body = format!("expected_user_id={}", user.id);
    let today = call_at_origin(&app, origin, GetToday::PATH, &request_body, Some(&session)).await;
    assert_eq!(today.status(), StatusCode::OK);

    let logout = call_at_origin(&app, origin, "/api/logout", "", Some(&session)).await;
    assert!(logout.status().is_success());
    let header = logout.headers()[header::SET_COOKIE].to_str().unwrap();
    let cookie = tower_sessions::cookie::Cookie::parse(header).unwrap();
    assert_eq!(cookie.name(), "minihabits_session");
    assert!(!cookie.secure().unwrap_or(false));
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(
        cookie.same_site(),
        Some(tower_sessions::cookie::SameSite::Lax)
    );
    assert_eq!(cookie.path(), Some("/"));
    assert!(cookie.domain().is_none());
    assert!(cookie.value().is_empty());
    assert_eq!(cookie.max_age(), Some(time::Duration::ZERO));
    let today = call_at_origin(&app, origin, GetToday::PATH, &request_body, Some(&session)).await;
    assert_eq!(today.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn real_endpoints_require_sessions_and_isolate_owned_data() {
    let (_directory, store, app) = test_app().await;
    let (alice, alice_cookie) = register(&app, "alice").await;
    let (bob, bob_cookie) = add_member(&app, &alice, &alice_cookie, "bob").await;
    let id = uuid::Uuid::new_v4().to_string();
    store
        .save_system(
            alice.id.clone(),
            id.clone(),
            SystemDraft {
                name: "Alice private system".into(),
                anchor: "After coffee".into(),
                floor: "One step".into(),
                lift: "Ten steps".into(),
            },
            "UTC".into(),
        )
        .await
        .unwrap();
    let day = chrono::Utc::now().date_naive();
    let week = minihabits_rust::domain::week_start_of(day);
    store
        .save_reflection(
            alice.id.clone(),
            Reflection {
                week_start: week,
                worked: "Alice private reflection".into(),
                obstacles: String::new(),
                adjustment: String::new(),
            },
        )
        .await
        .unwrap();
    store
        .save_reflection(
            bob.id.clone(),
            Reflection {
                week_start: week,
                worked: "Bob existing reflection".into(),
                obstacles: String::new(),
                adjustment: String::new(),
            },
        )
        .await
        .unwrap();
    for (path, form) in [
        (GetToday::PATH, format!("expected_user_id={}", alice.id)),
        (GetEditor::PATH, format!("expected_user_id={}", alice.id)),
        (
            GetWeeklyReview::PATH,
            format!("expected_user_id={}", alice.id),
        ),
        (SetArchived::PATH, format!("system_id={id}&archived=true")),
        (
            SetCheckIn::PATH,
            format!("system_id={id}&tier=floor&expected_day={day}"),
        ),
        (
            SaveSystem::PATH,
            format!(
                "id={id}&draft[name]=Forged&draft[anchor]=After&draft[floor]=One&draft[lift]=Ten&expected_user_id={}",
                alice.id
            ),
        ),
        (
            SaveReflection::PATH,
            format!(
                "reflection[week_start]={week}&reflection[worked]=Forged&reflection[obstacles]=&reflection[adjustment]=&expected_user_id={}",
                alice.id
            ),
        ),
    ] {
        let response = call(&app, path, &form, None).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "endpoint {path}: {}",
            body(response).await
        );
    }
    let alice_today = body(
        call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", alice.id),
            Some(&alice_cookie),
        )
        .await,
    )
    .await;
    assert!(alice_today.contains("Alice private system"));
    let bob_today = body(
        call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", bob.id),
            Some(&bob_cookie),
        )
        .await,
    )
    .await;
    assert!(!bob_today.contains("Alice private system"));
    let bob_review = body(
        call(
            &app,
            GetWeeklyReview::PATH,
            &format!("expected_user_id={}", bob.id),
            Some(&bob_cookie),
        )
        .await,
    )
    .await;
    assert!(!bob_review.contains("Alice private reflection"));
    for (path, form) in [
        (
            GetEditor::PATH,
            format!("system_id={id}&expected_user_id={}", bob.id),
        ),
        (SetArchived::PATH, format!("system_id={id}&archived=true")),
        (
            SetCheckIn::PATH,
            format!("system_id={id}&tier=lift&expected_day={day}"),
        ),
        (
            SaveSystem::PATH,
            format!(
                "id={id}&draft[name]=Forged&draft[anchor]=After&draft[floor]=One&draft[lift]=Ten&expected_user_id={}",
                bob.id
            ),
        ),
    ] {
        let response = call(&app, path, &form, Some(&bob_cookie)).await;
        assert!(!response.status().is_success(), "forged ID allowed: {path}");
        assert!(!body(response).await.contains("Alice private system"));
    }
    for path in [GetToday::PATH, GetEditor::PATH, GetWeeklyReview::PATH] {
        assert_eq!(
            call(
                &app,
                path,
                &format!("expected_user_id={}", alice.id),
                Some(&bob_cookie)
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let stale_reflection = format!(
        "reflection[week_start]={week}&reflection[worked]=Alice+stale+draft&reflection[obstacles]=&reflection[adjustment]=&expected_user_id={}",
        alice.id
    );
    assert_eq!(
        call(
            &app,
            SaveReflection::PATH,
            &stale_reflection,
            Some(&bob_cookie)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let fresh_id = uuid::Uuid::new_v4();
    let stale_system = format!(
        "id={fresh_id}&draft[name]=Stale&draft[anchor]=After&draft[floor]=One&draft[lift]=Ten&expected_user_id={}",
        alice.id
    );
    assert_eq!(
        call(&app, SaveSystem::PATH, &stale_system, Some(&bob_cookie))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let bob_data = store.snapshot(bob.id).await.unwrap();
    assert!(bob_data.systems.is_empty());
    assert_eq!(bob_data.reflections.len(), 1);
    assert_eq!(bob_data.reflections[0].worked, "Bob existing reflection");
    let alice_data = store.snapshot(alice.id).await.unwrap();
    assert_eq!(alice_data.systems[0].name, "Alice private system");
    assert_eq!(alice_data.reflections[0].worked, "Alice private reflection");
}

#[tokio::test]
async fn login_rotates_cookie_and_logout_revokes_only_that_device() {
    let (_directory, _store, app) = test_app().await;
    let (user, old_cookie) = register(&app, "alice").await;
    let incorrect = call(
        &app,
        "/api/login",
        "username=alice&password=incorrect-password-2026",
        None,
    )
    .await;
    let missing = call(
        &app,
        "/api/login",
        "username=unknown&password=incorrect-password-2026",
        None,
    )
    .await;
    assert_eq!(body(incorrect).await, body(missing).await);
    let form = "username=alice&password=synthetic-test-password-2026";
    let response = call(&app, "/api/login", form, Some(&old_cookie)).await;
    assert!(response.status().is_success());
    let cookie = session_cookie(&response);
    // Registration already authenticated this browser. axum-login's default
    // first-login protection alone would not rotate this existing session.
    assert!(
        cookie != old_cookie,
        "successful login must rotate an already-authenticated session"
    );
    assert_eq!(
        call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", user.id),
            Some(&old_cookie)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    let second = call(&app, "/api/login", form, None).await;
    assert!(second.status().is_success());
    let second_cookie = session_cookie(&second);
    assert!(
        cookie != second_cookie,
        "independent browsers must have independent session identifiers"
    );
    assert_eq!(today_status(&app, &user, &cookie).await, StatusCode::OK);
    assert_eq!(
        today_status(&app, &user, &second_cookie).await,
        StatusCode::OK
    );
    let logout = call(&app, "/api/logout", "", Some(&cookie)).await;
    assert!(logout.status().is_success());
    assert_session_cookie_removed(&logout);
    assert_eq!(
        call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", user.id),
            Some(&cookie)
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(
        call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", user.id),
            Some(&second_cookie)
        )
        .await
        .status()
        .is_success()
    );
}

#[tokio::test]
async fn switching_accounts_rotates_sessions_and_closed_registration_preserves_them() {
    let (_directory, store, app) = test_app().await;
    let (alice, alice_cookie) = register(&app, "alice").await;
    let (bob, original_bob_cookie) = add_member(&app, &alice, &alice_cookie, "bob").await;
    let week = minihabits_rust::domain::week_start_of(chrono::Utc::now().date_naive());
    for user in [&alice, &bob] {
        store
            .save_reflection(
                user.id.clone(),
                Reflection {
                    week_start: week,
                    worked: format!("{} preserved reflection", user.username),
                    obstacles: String::new(),
                    adjustment: String::new(),
                },
            )
            .await
            .unwrap();
    }

    // The browser currently belongs to Alice; signing in as Bob must not reuse
    // Alice's bearer identifier, and Bob's other device must stay independent.
    let response = call(
        &app,
        "/api/login",
        "username=bob&password=synthetic-test-password-2026",
        Some(&alice_cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_hardened_session_cookie(&response);
    let switched_cookie = session_cookie(&response);
    assert!(
        switched_cookie != alice_cookie && switched_cookie != original_bob_cookie,
        "cross-account login must issue an independent session identifier"
    );
    assert_eq!(
        serde_json::from_str::<User>(&body(response).await).unwrap(),
        bob
    );
    assert_eq!(
        today_status(&app, &alice, &alice_cookie).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        today_status(&app, &bob, &switched_cookie).await,
        StatusCode::OK
    );
    assert_eq!(
        today_status(&app, &alice, &switched_cookie).await,
        StatusCode::UNAUTHORIZED
    );
    let response = call(&app, "/api/current_user", "", Some(&switched_cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Option<User>>(&body(response).await).unwrap(),
        Some(bob.clone())
    );

    // Public registration stays closed even for an already authenticated user,
    // and a rejected signup must not replace their active session.
    let response = call(
        &app,
        "/api/register",
        "username=carol&password=synthetic-test-password-2026",
        Some(&switched_cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!response.headers().contains_key(header::SET_COOKIE));
    assert_eq!(
        today_status(&app, &bob, &switched_cookie).await,
        StatusCode::OK
    );
    let response = call(&app, "/api/current_user", "", Some(&switched_cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Option<User>>(&body(response).await).unwrap(),
        Some(bob.clone())
    );
    assert_eq!(
        today_status(&app, &bob, &original_bob_cookie).await,
        StatusCode::OK
    );
    for user in [&alice, &bob] {
        let snapshot = store.snapshot(user.id.clone()).await.unwrap();
        assert_eq!(snapshot.reflections.len(), 1);
        assert_eq!(
            snapshot.reflections[0].worked,
            format!("{} preserved reflection", user.username)
        );
    }
}

#[tokio::test]
async fn malformed_and_fabricated_session_cookies_never_authenticate() {
    let (_directory, _store, app) = test_app().await;
    let (user, valid_cookie) = register(&app, "alice").await;
    let fabricated = tower_sessions::session::Id::default().to_string();
    // Include the old custom token's shape, invalid base64, incorrect lengths,
    // and a syntactically valid library identifier that has no stored record.
    for value in [
        String::new(),
        "not-a-session".into(),
        "!".repeat(22),
        "a".repeat(64),
        fabricated,
    ] {
        let cookie = format!("__Host-minihabits_session={value}");
        let rejected = call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", user.id),
            Some(&cookie),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert_session_cookie_removed(&rejected);
        let response = call(&app, "/api/current_user", "", Some(&cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, "null");
    }
    assert_eq!(
        today_status(&app, &user, &valid_cookie).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn sessions_keep_absolute_expiry_and_expired_records_are_rejected() {
    let (directory, _store, app) = test_app().await;
    let (user, cookie) = register(&app, "alice").await;
    let connection = rusqlite::Connection::open(directory.path().join("test.sqlite3")).unwrap();
    let session_id = cookie.split_once('=').unwrap().1;
    let (expiry_before, expiry_timestamp): (String, i64) = connection
        .query_row(
            "SELECT expiry_date, CAST(strftime('%s', expiry_date) AS INTEGER) FROM tower_sessions WHERE id = ?1",
            [session_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    let seconds_left = expiry_timestamp - chrono::Utc::now().timestamp();
    assert!(
        (604_740..=604_800).contains(&seconds_left),
        "new sessions must expire after seven days"
    );
    for _ in 0..3 {
        assert_eq!(today_status(&app, &user, &cookie).await, StatusCode::OK);
    }
    let week = minihabits_rust::domain::week_start_of(chrono::Utc::now().date_naive());
    let reflection = format!(
        "reflection[week_start]={week}&reflection[worked]=Persisted&reflection[obstacles]=&reflection[adjustment]=&expected_user_id={}",
        user.id
    );
    assert_eq!(
        call(&app, SaveReflection::PATH, &reflection, Some(&cookie))
            .await
            .status(),
        StatusCode::OK
    );
    let expiry_after: String = connection
        .query_row(
            "SELECT expiry_date FROM tower_sessions WHERE id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        expiry_before, expiry_after,
        "authenticated reads and writes must not slide the absolute deadline"
    );

    // tower-sessions-sqlx-store declares INTEGER affinity but SQLx stores
    // OffsetDateTime as an ISO datetime string. Backdate the expiry column to
    // exercise the library's real expiry check, without changing the clock.
    assert_eq!(
        connection
            .execute(
                "UPDATE tower_sessions SET expiry_date = '2000-01-01 00:00:00+00:00' WHERE id = ?1",
                [session_id],
            )
            .unwrap(),
        1
    );
    let rejected = call(
        &app,
        GetToday::PATH,
        &format!("expected_user_id={}", user.id),
        Some(&cookie),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_session_cookie_removed(&rejected);
    let response = call(&app, "/api/current_user", "", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, "null");
}

#[tokio::test]
async fn sessions_survive_rebuilding_the_router_backend_and_database_connections() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("test.sqlite3");
    let (store, app) = app_at(&database).await;
    let (user, cookie) = register(&app, "alice").await;
    assert_eq!(today_status(&app, &user, &cookie).await, StatusCode::OK);
    drop(app);
    drop(store);

    let (_reopened_store, restarted_app) = app_at(&database).await;
    let response = call(&restarted_app, "/api/current_user", "", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<Option<User>>(&body(response).await).unwrap(),
        Some(user.clone())
    );
    assert_eq!(
        today_status(&restarted_app, &user, &cookie).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn changing_the_password_hash_invalidates_existing_device_sessions() {
    use argon2::{Argon2, PasswordHasher, password_hash::SaltString};

    let (directory, _store, app) = test_app().await;
    let (user, first_cookie) = register(&app, "alice").await;
    let second = call(
        &app,
        "/api/login",
        "username=alice&password=synthetic-test-password-2026",
        None,
    )
    .await;
    assert_eq!(second.status(), StatusCode::OK);
    let second_cookie = session_cookie(&second);
    assert_eq!(
        today_status(&app, &user, &first_cookie).await,
        StatusCode::OK
    );
    assert_eq!(
        today_status(&app, &user, &second_cookie).await,
        StatusCode::OK
    );

    // This is an intentionally public synthetic fixture, never a real secret.
    let salt = SaltString::encode_b64(b"public-test-fixture-salt").unwrap();
    let replacement_hash = Argon2::default()
        .hash_password(b"replacement-public-fixture-password", &salt)
        .unwrap()
        .to_string();
    let connection = rusqlite::Connection::open(directory.path().join("test.sqlite3")).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE users SET password_hash = ?1 WHERE id = ?2",
                rusqlite::params![replacement_hash, user.id],
            )
            .unwrap(),
        1
    );
    for cookie in [&first_cookie, &second_cookie] {
        let rejected = call(
            &app,
            GetToday::PATH,
            &format!("expected_user_id={}", user.id),
            Some(cookie),
        )
        .await;
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert_session_cookie_removed(&rejected);
        let response = call(&app, "/api/current_user", "", Some(cookie)).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, "null");
    }
    let old_password = call(
        &app,
        "/api/login",
        "username=alice&password=synthetic-test-password-2026",
        None,
    )
    .await;
    assert!(!old_password.status().is_success());
    let fresh = call(
        &app,
        "/api/login",
        "username=alice&password=replacement-public-fixture-password",
        None,
    )
    .await;
    assert_eq!(fresh.status(), StatusCode::OK);
    assert_eq!(
        today_status(&app, &user, &session_cookie(&fresh)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn origin_and_host_guards_reject_requests_before_mutating_sessions() {
    let (_directory, _store, app) = test_app().await;
    let (user, cookie) = register(&app, "alice").await;
    for (host, origin) in [
        (None, Some("https://habits.test")),
        (Some("evil.test"), Some("https://habits.test")),
        (Some("habits.test"), None),
        (Some("habits.test"), Some("null")),
        (Some("habits.test"), Some("https://evil.test")),
        (Some("habits.test"), Some("https://habits.test.evil.test")),
        (Some("habits.test"), Some("http://habits.test")),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/logout")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .header(header::COOKIE, &cookie)
            // Proxy headers must not override the canonical Host/Origin guard.
            .header("x-forwarded-host", "habits.test")
            .header("x-forwarded-proto", "https");
        if let Some(host) = host {
            request = request.header(header::HOST, host);
        }
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        assert_eq!(today_status(&app, &user, &cookie).await, StatusCode::OK);
    }
    for extra_header in [header::HOST, header::ORIGIN] {
        let value = if extra_header == header::HOST {
            "habits.test"
        } else {
            "https://habits.test"
        };
        let request = Request::builder()
            .method("POST")
            .uri("/api/logout")
            .header(header::HOST, "habits.test")
            .header(header::ORIGIN, "https://habits.test")
            .header(header::COOKIE, &cookie)
            .header(extra_header, value)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    let request = Request::builder()
        .method("POST")
        .uri("/api/logout")
        .header(header::HOST, "habits.test")
        .header(header::ORIGIN, "https://habits.test")
        .header("sec-fetch-site", "cross-site")
        .header(header::COOKIE, &cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(today_status(&app, &user, &cookie).await, StatusCode::OK);
}

#[tokio::test]
async fn duplicate_session_cookies_fail_closed_even_when_the_values_match() {
    let (_directory, _store, app) = test_app().await;
    let (alice, alice_cookie) = register(&app, "alice").await;
    let (bob, bob_cookie) = add_member(&app, &alice, &alice_cookie, "bob").await;
    let malformed_cookie = "__Host-minihabits_session=not-a-session";
    for duplicate in [&alice_cookie, &bob_cookie, malformed_cookie] {
        for separate_headers in [false, true] {
            for reverse in [false, true] {
                let (first, second) = if reverse {
                    (duplicate, alice_cookie.as_str())
                } else {
                    (alice_cookie.as_str(), duplicate)
                };
                let mut request = Request::builder()
                    .method("POST")
                    .uri("/api/logout")
                    .header(header::HOST, "habits.test")
                    .header(header::ORIGIN, "https://habits.test")
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
                request = if separate_headers {
                    request
                        .header(header::COOKIE, first)
                        .header(header::COOKIE, second)
                } else {
                    request.header(header::COOKIE, format!("{first}; {second}"))
                };
                let response = app
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert!(!response.headers().contains_key(header::SET_COOKIE));
            }
        }
    }
    assert_eq!(
        today_status(&app, &alice, &alice_cookie).await,
        StatusCode::OK
    );
    assert_eq!(today_status(&app, &bob, &bob_cookie).await, StatusCode::OK);

    let unrelated_cookies = format!("theme=light; {alice_cookie}; theme=dark");
    assert_eq!(
        today_status(&app, &alice, &unrelated_cookies).await,
        StatusCode::OK,
        "unrelated cookie names must not be mistaken for ambiguous sessions"
    );
}

#[tokio::test]
async fn native_post_login_sets_cookie_without_credentials_in_redirect() {
    let (_directory, _store, app) = test_app().await;
    let (_user, _cookie) = register(&app, "native_form").await;
    let request = Request::builder()
        .method("POST")
        .uri("/api/login")
        .header(header::HOST, "habits.test")
        .header(header::ORIGIN, "https://habits.test")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .header(header::ACCEPT, "text/html")
        .header(header::REFERER, "https://habits.test/login")
        .body(Body::from(
            "username=native_form&password=synthetic-test-password-2026",
        ))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .trim_end_matches('?'),
        "https://habits.test/login"
    );
    assert_hardened_session_cookie(&response);
    assert!(!body(response).await.contains("synthetic-test-password"));
}
