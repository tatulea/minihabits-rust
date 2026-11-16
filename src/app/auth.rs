//! Account screens and the session boundary. A server-verified session, never a
//! browser preference, decides whether private features are mounted.
use super::*;
use crate::{
    auth::User,
    domain::{Reflection, SystemDraft},
    server::{current_user, login, logout, register, registration_open},
};
use leptos_router::{components::Redirect, hooks::use_navigate};

pub(super) const SIGN_IN_REQUIRED: &str = "Please sign in to continue.";

#[derive(Clone)]
pub(super) struct BuilderRecovery {
    pub owner_id: String,
    pub system_id: String,
    pub is_edit: bool,
    pub draft: SystemDraft,
}
#[derive(Clone)]
pub(super) struct ReflectionRecovery {
    pub owner_id: String,
    pub reflection: Reflection,
}
#[derive(Clone, Copy)]
pub(super) struct Auth {
    pub user: Resource<Result<Option<User>, ServerFnError>>,
    pub registration: Resource<Result<bool, ServerFnError>>,
    locked: RwSignal<bool>,
    pub revision: RwSignal<u64>,
    busy: RwSignal<bool>,
    pub builder_recovery: RwSignal<Option<BuilderRecovery>>,
    pub reflection_recovery: RwSignal<Option<ReflectionRecovery>>,
}
impl Auth {
    pub fn new() -> Self {
        let auth = Self {
            user: Resource::new_blocking(|| (), |_| current_user()),
            registration: Resource::new_blocking(|| (), |_| registration_open()),
            locked: RwSignal::new(false),
            revision: RwSignal::new(0),
            busy: RwSignal::new(false),
            builder_recovery: RwSignal::new(None),
            reflection_recovery: RwSignal::new(None),
        };
        #[cfg(feature = "hydrate")]
        {
            // Resource reads stay in a reactive owner, never timer callbacks.
            // Observe changes made by another tab sharing the session cookie.
            let observed_identity = StoredValue::new(None::<Option<String>>);
            Effect::new(move |_| {
                if let Some(Ok(user)) = auth.user.get() {
                    let identity = user.as_ref().map(|user| user.id.clone());
                    if observed_identity
                        .get_value()
                        .is_some_and(|previous| previous != identity)
                    {
                        auth.revision.update(|revision| *revision += 1);
                    }
                    observed_identity.set_value(Some(identity));
                    if let Some(user) = user {
                        auth.builder_recovery.update(|draft| {
                            if draft
                                .as_ref()
                                .is_some_and(|draft| draft.owner_id != user.id)
                            {
                                *draft = None;
                            }
                        });
                        auth.reflection_recovery.update(|draft| {
                            if draft
                                .as_ref()
                                .is_some_and(|draft| draft.owner_id != user.id)
                            {
                                *draft = None;
                            }
                        });
                    }
                }
            });
            let focus = window_event_listener(leptos::ev::focus, move |_| {
                if !auth.locked.get_untracked() && !auth.busy.get_untracked() {
                    auth.user.refetch();
                    auth.registration.refetch();
                }
            });
            on_cleanup(move || focus.remove());
            if let Ok(timer) = set_interval_with_handle(
                move || {
                    if !auth.locked.get_untracked() && !auth.busy.get_untracked() {
                        auth.user.refetch();
                        auth.registration.refetch();
                    }
                },
                std::time::Duration::from_secs(30),
            ) {
                on_cleanup(move || timer.clear());
            }
        }
        auth
    }
    pub fn current(&self) -> Option<User> {
        if self.locked.get() {
            None
        } else {
            self.user.get().and_then(Result::ok).flatten()
        }
    }
    /// Snapshot the already-verified identity when a guarded feature mounts.
    /// This is intentionally not a subscription: RequireAuth owns identity
    /// changes and recreates the feature under a new account/session key.
    pub fn owner_id(&self) -> String {
        if self.locked.get_untracked() {
            String::new()
        } else {
            self.user
                .get_untracked()
                .and_then(Result::ok)
                .flatten()
                .map(|user| user.id)
                .unwrap_or_default()
        }
    }
    pub fn clear_builder_recovery(&self, saved: &BuilderRecovery) {
        self.builder_recovery.update(|draft| {
            if draft.as_ref().is_some_and(|draft| {
                draft.owner_id == saved.owner_id
                    && draft.system_id == saved.system_id
                    && draft.draft == saved.draft
            }) {
                *draft = None;
            }
        });
    }
    pub fn clear_reflection_recovery(&self, saved: &ReflectionRecovery) {
        self.reflection_recovery.update(|draft| {
            if draft.as_ref().is_some_and(|draft| {
                draft.owner_id == saved.owner_id && draft.reflection == saved.reflection
            }) {
                *draft = None;
            }
        });
    }
    pub fn authenticated(&self, user: User) {
        batch(|| {
            // Apply the identity and its view generation as one transition.
            // Never mount old-account children under a new revision.
            self.locked.set(true);
            self.builder_recovery.update(|draft| {
                if draft
                    .as_ref()
                    .is_some_and(|draft| draft.owner_id != user.id)
                {
                    *draft = None;
                }
            });
            self.reflection_recovery.update(|draft| {
                if draft
                    .as_ref()
                    .is_some_and(|draft| draft.owner_id != user.id)
                {
                    *draft = None;
                }
            });
            self.user.set(Some(Ok(Some(user))));
            self.revision.update(|revision| *revision += 1);
            self.locked.set(false);
        });
        self.user.refetch();
        self.registration.refetch();
    }
    pub fn expire(&self) {
        batch(|| {
            // Lock before changing the revision: no transient remount of the
            // previous account while its authenticated owner is removed.
            self.locked.set(true);
            self.user.set(Some(Ok(None)));
            self.revision.update(|revision| *revision += 1);
        });
    }
    fn signed_out(&self) {
        self.expire();
        self.builder_recovery.set(None);
        self.reflection_recovery.set(None);
    }
}

pub(super) fn is_auth_error(error: &ServerFnError) -> bool {
    matches!(error, ServerFnError::ServerError(message) if message == SIGN_IN_REQUIRED)
}

#[component]
pub(super) fn RequireAuth(children: ChildrenFn) -> impl IntoView {
    let auth = expect_context::<Auth>();
    view! {
        <Suspense fallback=|| view! {<p class="mh-today__hint" role="status">"Checking your session…"</p>}>
            <Show when=move || auth.current().is_none()>
                <Show when=move || auth.locked.get() || !matches!(auth.user.get(), Some(Err(_)))>
                    <Entry/>
                </Show>
                {move || if auth.locked.get() {
                    None
                } else {
                    auth.user.get().and_then(Result::err).map(|error| view! {
                        <LoadError message=friendly_error(&error) retry=Callback::new(move |_| auth.user.refetch())/>
                    })
                }}
            </Show>
            <For each={move || auth.current().map(|user| (user.id, auth.revision.get())).into_iter().collect::<Vec<_>>()} key=|session| session.clone() children=move |_| children()/>
        </Suspense>
    }
}

#[component]
pub(super) fn AccountActions() -> impl IntoView {
    let auth = expect_context::<Auth>();
    let notice = expect_context::<Notice>().0;
    let refresh = expect_context::<Refresh>().0;
    let busy = auth.busy;
    let tasks = ViewTasks::new();
    let navigate = use_navigate();
    view! {
        <Suspense fallback=|| ()>
            {move || auth.current().map(|user| view! {
                <span class="mh-topbar__user">{user.username}</span>
            })}
            <Show when=move || auth.current().is_some_and(|user| user.is_admin)>
                <a href="/app/accounts" class="mh-btn mh-btn--ghost mh-focusable">"Accounts"</a>
            </Show>
            <Show when=move || auth.current().is_some()>
                <button type="button" class="mh-btn mh-btn--ghost mh-focusable" disabled=move || busy.get() on:click={let navigate = navigate.clone(); move |_| {
                    if busy.get_untracked() { return; }
                    busy.set(true);
                    let navigate = navigate.clone();
                    tasks.spawn(move |active| async move {
                        match logout().await {
                            Ok(()) => {
                                auth.signed_out();
                                refresh.update(|value| *value += 1);
                                notice.set("You are signed out.".into());
                                if active.is_active() { navigate("/login", Default::default()); }
                            }
                            Err(_) => notice.set("We could not sign you out. Please try again.".into()),
                        }
                        busy.set(false);
                    });
                }}>{move || if busy.get() { "Please wait…" } else { "Sign out" }}</button>
            </Show>
        </Suspense>
    }
}

#[component]
pub(super) fn Entry() -> impl IntoView {
    let auth = expect_context::<Auth>();
    view! {
        <Suspense fallback=|| view! {<p role="status">"Loading…"</p>}>
            <Show when=move || auth.current().is_some()>
                <Redirect path="/app"/>
            </Show>
            <Show when=move || auth.current().is_none()>
                <Show when=move || matches!(auth.registration.get(), Some(Ok(true)))>
                    <AccountForm registering=true/>
                </Show>
                <Show when=move || matches!(auth.registration.get(), Some(Ok(false)))>
                    <AccountForm registering=false/>
                </Show>
                {move || auth.registration.get().and_then(Result::err).map(|error| view! {
                    <LoadError message=friendly_error(&error) retry=Callback::new(move |_| auth.registration.refetch())/>
                })}
            </Show>
        </Suspense>
    }
}

#[component]
fn AccountForm(registering: bool) -> impl IntoView {
    let auth = expect_context::<Auth>();
    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    let busy = auth.busy;
    let error = RwSignal::new(String::new());
    let notice = expect_context::<Notice>().0;
    let refresh = expect_context::<Refresh>().0;
    let tasks = ViewTasks::new();
    let navigate = use_navigate();
    view! {
        <section class="mh-auth">
            <header class="mh-today__head">
                <p class="mh-today__eyebrow">"minihabits"</p>
                <h1 class="mh-today__title">{if registering {"Set up MiniHabits"} else {"Welcome back."}}</h1>
                <p class="mh-today__lead">{if registering {"Create the administrator account. You can add other accounts after signing in."} else {"Your systems are right where you left them. Showing up counts."}}</p>
            </header>
            <form class="mh-builder__form mh-auth__form" method="post" action=if registering {"/api/register"} else {"/api/login"} aria-label=if registering {"Create an account"} else {"Sign in"} aria-busy=move || busy.get().to_string() novalidate on:submit=move |event| {
                event.prevent_default();
                if busy.get_untracked() { return; }
                let username_value = username.get_untracked().trim().to_string();
                let password_value = password.get_untracked();
                if username_value.len() < 3 || username_value.len() > 32 || !username_value.bytes().all(|value| value.is_ascii_alphanumeric() || value == b'_') {
                    error.set("Use a username of 3–32 letters, numbers, or underscores.".into()); return;
                }
                if !(15..=128).contains(&password_value.chars().count()) {
                    error.set("Use a password of 15–128 characters.".into()); return;
                }
                if registering && password_value != confirmation.get_untracked() {
                    error.set("Your passwords do not match.".into()); return;
                }
                error.set(String::new());
                busy.set(true);
                let navigate = navigate.clone();
                tasks.spawn(move |active| async move {
                    let result = if registering {register(username_value, password_value).await} else {login(username_value, password_value).await};
                    match result {
                        Ok(user) => {
                            auth.authenticated(user);
                            refresh.update(|value| *value += 1);
                            notice.set(if registering {"Your administrator account is ready."} else {"Welcome back."}.into());
                            if active.is_active() {
                                password.set(String::new());
                                confirmation.set(String::new());
                                navigate("/app", Default::default());
                            }
                        }
                        Err(failure) => if active.is_active() {
                            if registering { auth.registration.refetch(); }
                            error.set(friendly_error(&failure));
                        },
                    }
                    busy.set(false);
                });
            }>
                <div class="mh-field">
                    <label for="account-username" class="mh-field__label">"Username"</label>
                    <p id="account-username-help" class="mh-field__help">"3–32 letters, numbers, or underscores. No spaces."</p>
                    <input id="account-username" name="username" type="text" autocomplete="username" autocapitalize="none" spellcheck="false" required minlength="3" maxlength="32" class="mh-input mh-focusable" aria-describedby="account-username-help" disabled=move || busy.get() prop:value=move || username.get() on:input=move |event| username.set(event_target_value(&event))/>
                </div>
                <div class="mh-field">
                    <label for="account-password" class="mh-field__label">"Password"</label>
                    <p id="account-password-help" class="mh-field__help">{if registering {"15–128 characters. A unique passphrase works well."} else {"Enter the password you chose for this account."}}</p>
                    <input id="account-password" name="password" type="password" autocomplete=if registering {"new-password"} else {"current-password"} required minlength="15" maxlength="256" class="mh-input mh-focusable" aria-describedby="account-password-help" disabled=move || busy.get() prop:value=move || password.get() on:input=move |event| password.set(event_target_value(&event))/>
                </div>
                {registering.then(|| view! {
                    <div class="mh-field">
                        <label for="account-confirmation" class="mh-field__label">"Confirm password"</label>
                        <input id="account-confirmation" name="password-confirmation" type="password" autocomplete="new-password" required minlength="15" maxlength="256" class="mh-input mh-focusable" disabled=move || busy.get() prop:value=move || confirmation.get() on:input=move |event| confirmation.set(event_target_value(&event))/>
                    </div>
                })}
                {move || (!error.get().is_empty()).then(|| view! {<p class="mh-field__error" role="alert">{move || error.get()}</p>})}
                <div class="mh-builder__actions">
                    <button type="submit" class="mh-btn mh-btn--primary mh-focusable" disabled=move || busy.get()>{move || if busy.get() {"Please wait…"} else if registering {"Create account"} else {"Sign in"}}</button>
                </div>
                <p class="mh-local-note">"Your account and habits are stored on this server."</p>
            </form>
        </section>
    }.into_any()
}
