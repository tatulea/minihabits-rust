#![recursion_limit = "256"]
#[cfg(feature = "ssr")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use axum::{Router, extract::DefaultBodyLimit, middleware};
    use leptos::prelude::*;
    use leptos_axum::{LeptosRoutes, generate_route_list};
    use minihabits_rust::{
        app::{App, shell},
        auth::{AuthService, session_layer},
        domain::today_in_timezone,
        security::{SecurityConfig, request_guard},
        server::AppContext,
        store::Store,
    };
    use std::{net::SocketAddr, path::Path};

    let database =
        std::env::var("MINIHABITS_DATABASE").unwrap_or_else(|_| "data/minihabits.sqlite3".into());
    let store = Store::open(Path::new(&database))?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Local administrative maintenance only. Existing data is never assigned by
    // registration; a server operator must name its intended owner explicitly.
    if args.len() == 2 && args[0] == "import-legacy" {
        store.import_legacy(args[1].clone()).await?;
        println!("Legacy data imported to the named account. Account passwords were not changed.");
        return Ok(());
    }
    if !args.is_empty() {
        return Err("Usage: minihabits-rust [import-legacy USERNAME]".into());
    }
    let security = SecurityConfig::from_env()?;
    let conf = get_configuration(Some("Cargo.toml"))?;
    let options = conf.leptos_options;
    let addr = options.site_addr;
    security.validate_bind_addr(addr)?;
    let time_zone = std::env::var("TZ").unwrap_or_else(|_| "UTC".into());
    today_in_timezone(&time_zone)?;
    let auth = AuthService::new(store.clone()).await?;
    let sessions = session_layer(Path::new(&database), &security).await?;
    let auth_layer = axum_login::AuthManagerLayerBuilder::new(auth.clone(), sessions).build();
    let ctx = AppContext {
        store,
        time_zone,
        auth,
    };
    let routes = generate_route_list(App);
    let context = move || provide_context(ctx.clone());
    let app = Router::new()
        .leptos_routes_with_context(&options, routes, context.clone(), {
            let options = options.clone();
            move || shell(options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler_with_context(
            context, shell,
        ))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(auth_layer)
        .layer(middleware::from_fn_with_state(
            security.clone(),
            request_guard,
        ))
        .with_state(options);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!(
        "MiniHabits: {} | listening {addr} | authentication required | SQLite: {database}",
        security.origin
    );
    if !security.secure_cookies {
        eprintln!(
            "Local HTTP development: passwords and sessions are unencrypted. Use an HTTPS proxy for deployment."
        );
    }
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

#[cfg(not(feature = "ssr"))]
fn main() {}
