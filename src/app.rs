//! Application frame and shared presentation primitives. Feature modules own
//! their forms and loading states; no persistence rules live in components.
mod accounts;
mod auth;
mod builder;
mod review;
mod today;
use crate::domain::{DayState, MomentumDay};
use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::{
    components::{Route, Router, Routes},
    hooks::use_location,
    path,
};

#[derive(Clone, Copy)]
pub(crate) struct Refresh(pub RwSignal<u64>);
#[derive(Clone, Copy)]
pub(crate) struct Notice(pub RwSignal<String>);

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en" data-theme="light">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                <meta name="color-scheme" content="light dark"/>
                <AutoReload options=options.clone()/>
                <HydrationScripts options/>
                <MetaTags/>
            </head>
            <body><App/></body>
        </html>
    }
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    provide_context(Refresh(RwSignal::new(0)));
    provide_context(Notice(RwSignal::new(String::new())));
    provide_context(auth::Auth::new());
    view! {
        <Stylesheet id="leptos" href="/app.css"/>
        <Title text="minihabits · a little, every day"/>
        <Router>
            <Frame/>
        </Router>
    }
}

#[component]
fn Frame() -> impl IntoView {
    let auth = expect_context::<auth::Auth>();
    let dark = RwSignal::new(false);
    let notice = expect_context::<Notice>().0;
    let location = use_location();
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if !notice.get().is_empty()
            && let Ok(timer) = set_timeout_with_handle(
                move || notice.set(String::new()),
                std::time::Duration::from_secs(8),
            )
        {
            on_cleanup(move || timer.clear());
        }
    });
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(window) = web_sys::window()
            && let Ok(Some(storage)) = window.local_storage()
            && let Ok(Some(theme)) = storage.get_item("minihabits-theme")
        {
            dark.set(theme == "dark");
        }
    });
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(window) = web_sys::window()
            && let Some(document) = window.document()
            && let Some(root) = document.document_element()
        {
            let _ = root.set_attribute("data-theme", if dark.get() { "dark" } else { "light" });
        }
    });
    let toggle_theme = move |_| {
        dark.update(|value| *value = !*value);
        #[cfg(feature = "hydrate")]
        if let Some(window) = web_sys::window()
            && let Ok(Some(storage)) = window.local_storage()
        {
            let _ = storage.set_item(
                "minihabits-theme",
                if dark.get_untracked() {
                    "dark"
                } else {
                    "light"
                },
            );
        }
    };
    view! {
        <div class="mh-root mh-web" data-theme=move || if dark.get() {"dark"} else {"light"}>
            <a href="#main" class="mh-skip-link">"Skip to content"</a>
            <header class="mh-topbar">
                <a href="/app" class="mh-topbar__brand mh-focusable">"minihabits"</a>
                <Suspense fallback=|| ()>
                    <Show when=move || auth.current().is_some()>
                        <nav class="mh-nav mh-nav--web" aria-label="Primary">
                            <a href="/app" class=move || nav_class(location.pathname.get() == "/app" || location.pathname.get() == "/")>"Today"</a>
                            <a href="/app/design" class=move || nav_class(location.pathname.get().contains("design") || location.pathname.get().contains("systems"))>"Design a system"</a>
                            <a href="/app/review" class=move || nav_class(location.pathname.get() == "/app/review")>"Weekly review"</a>
                        </nav>
                    </Show>
                </Suspense>
                <div class="mh-topbar__actions">
                    <auth::AccountActions/>
                    <button type="button" class="mh-btn mh-btn--ghost mh-focusable" on:click=toggle_theme aria-label=move || if dark.get() {"Switch to light theme"} else {"Switch to dark theme"}>
                        {move || if dark.get() {"Light theme"} else {"Dark theme"}}
                    </button>
                </div>
            </header>
            <main class="mh-web__main" id="main">
                <Routes fallback=NotFound>
                    <Route path=path!("") view=|| view! {<auth::RequireAuth><today::Today/></auth::RequireAuth>}/>
                    <Route path=path!("/app") view=|| view! {<auth::RequireAuth><today::Today/></auth::RequireAuth>}/>
                    <Route path=path!("/app/design") view=|| view! {<auth::RequireAuth><builder::Builder/></auth::RequireAuth>}/>
                    <Route path=path!("/app/systems/:id/edit") view=|| view! {<auth::RequireAuth><builder::Builder/></auth::RequireAuth>}/>
                    <Route path=path!("/app/review") view=|| view! {<auth::RequireAuth><review::Review/></auth::RequireAuth>}/>
                    <Route path=path!("/app/accounts") view=|| view! {<auth::RequireAuth><accounts::Accounts/></auth::RequireAuth>}/>
                    <Route path=path!("/login") view=auth::Entry/>
                    <Route path=path!("/register") view=auth::Entry/>
                </Routes>
            </main>
            <div class="mh-status" role="status" aria-live="polite">
                {move || (!notice.get().is_empty()).then(|| view! {
                    <span>{move || notice.get()}</span>
                    <button type="button" class="mh-status__close mh-focusable" aria-label="Dismiss notification" on:click=move |_| notice.set(String::new())>"×"</button>
                })}
            </div>
            <noscript><p class="mh-noscript">"Enable JavaScript for interactive check-ins. Your systems are safely stored on the server."</p></noscript>
        </div>
    }
}
fn nav_class(active: bool) -> &'static str {
    if active {
        "mh-nav__link mh-nav__link--active"
    } else {
        "mh-nav__link"
    }
}
#[component]
fn NotFound() -> impl IntoView {
    view! { <section class="mh-today"><h1>"This page is not here."</h1><a class="mh-btn mh-focusable" href="/app">"Back to Today"</a></section> }
}

#[component]
pub(crate) fn LoadError(message: String, retry: Callback<()>) -> impl IntoView {
    let session_expired = message == auth::SIGN_IN_REQUIRED;
    let auth = expect_context::<auth::Auth>();
    let notice = expect_context::<Notice>().0;
    Effect::new(move |_| {
        if session_expired {
            notice.set("Your session ended. Sign in again to continue.".into());
            auth.expire();
        }
    });
    view! { <div class="mh-error" role="alert"><p>{message}</p>
        {if session_expired {
            view! {<a class="mh-btn mh-btn--primary mh-focusable" href="/login">"Sign in again"</a>}.into_any()
        } else {
            view! {<button class="mh-btn mh-btn--ghost mh-focusable" type="button" on:click=move |_| retry.run(())>"Try again"</button>}.into_any()
        }}
    </div> }
}

pub(crate) fn friendly_error(error: &ServerFnError) -> String {
    match error {
        ServerFnError::ServerError(message) => message.clone(),
        _ => "We could not reach the app. Check the server is running and try again.".into(),
    }
}

pub(crate) fn state_name(state: DayState) -> &'static str {
    match state {
        DayState::Floor => "floor",
        DayState::Lift => "lift",
        DayState::Miss => "miss",
    }
}

#[component]
pub(crate) fn MomentumStrip(series: Vec<MomentumDay>, label: String) -> impl IntoView {
    let last = series.len().saturating_sub(1);
    view! {
        <div class="mh-strip" role="img" aria-label=label>
            {series.into_iter().enumerate().map(|(index, day)| {
                let state = state_name(day.state);
                let class = format!("mh-strip__cell mh-strip__cell--{state} {}", if index == last {"mh-strip__cell--today"} else {""});
                view! { <i class=class title=format!("{}: {}", day.day, match day.state {DayState::Floor => "Showed up", DayState::Lift => "Went full", DayState::Miss => "Not logged"})></i> }
            }).collect_view()}
        </div>
    }
}

/// Keep mutation completion separate from view lifetime. Writes are allowed to
/// finish after navigation so shared data can refresh, but late responses must
/// never navigate backwards or write to disposed form signals.
#[derive(Clone, Copy)]
pub(crate) struct ViewTasks(StoredValue<std::sync::Arc<std::sync::atomic::AtomicBool>>);
pub(crate) struct ViewAlive(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl ViewAlive {
    pub fn is_active(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl ViewTasks {
    pub fn new() -> Self {
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let cleanup = alive.clone();
        on_cleanup(move || cleanup.store(false, std::sync::atomic::Ordering::Relaxed));
        Self(StoredValue::new(alive))
    }
    pub fn spawn<F: std::future::Future<Output = ()> + 'static>(
        self,
        task: impl FnOnce(ViewAlive) -> F,
    ) {
        leptos::task::spawn_local(task(ViewAlive(self.0.get_value())));
    }
}
