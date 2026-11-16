#![recursion_limit = "256"]
pub mod app;
pub mod auth;
pub mod domain;
#[cfg(feature = "ssr")]
pub mod security;
pub mod server;
#[cfg(feature = "ssr")]
mod session;
#[cfg(feature = "ssr")]
pub mod store;

#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
