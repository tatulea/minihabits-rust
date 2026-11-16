use super::*;
use crate::server::{create_account, list_accounts};

#[component]
pub(super) fn Accounts() -> impl IntoView {
    let auth = expect_context::<auth::Auth>();
    view! {
        <Show when=move || auth.current().is_some_and(|user| user.is_admin) fallback=|| view! {
            <section class="mh-auth">
                <h1 class="mh-today__title">"Administrator access required"</h1>
                <a href="/app" class="mh-btn mh-focusable">"Back to Today"</a>
            </section>
        }>
            <AccountManager/>
        </Show>
    }
}

#[component]
fn AccountManager() -> impl IntoView {
    let auth = expect_context::<auth::Auth>();
    let owner = StoredValue::new(auth.owner_id());
    let accounts = Resource::new(move || (), move |_| list_accounts(owner.get_value()));
    let username = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let confirmation = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(String::new());
    let success = RwSignal::new(String::new());
    let tasks = ViewTasks::new();
    view! {
        <section class="mh-auth">
            <header class="mh-today__head">
                <h1 class="mh-today__title">"Accounts"</h1>
                <p class="mh-today__lead">"Create an account for someone who will use this server. Each account has its own habits."</p>
            </header>
            <form class="mh-builder__form mh-auth__form" method="post" action="/api/create_account" aria-label="Create an account" aria-busy=move || busy.get().to_string() on:submit=move |event| {
                event.prevent_default();
                if busy.get_untracked() { return; }
                error.set(String::new());
                success.set(String::new());
                let name = username.get_untracked().trim().to_string();
                let secret = password.get_untracked();
                if !(3..=32).contains(&name.len()) || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
                    error.set("Use a username of 3–32 letters, numbers, or underscores.".into()); return;
                }
                if !(15..=128).contains(&secret.chars().count()) || secret.len() > 512 {
                    error.set("Use a password of 15–128 characters.".into()); return;
                }
                if secret != confirmation.get_untracked() {
                    error.set("The passwords do not match.".into()); return;
                }
                busy.set(true);
                tasks.spawn(move |active| async move {
                    let result = create_account(owner.get_value(), name, secret).await;
                    if !active.is_active() { return; }
                    password.set(String::new());
                    confirmation.set(String::new());
                    busy.set(false);
                    match result {
                        Ok(user) => {
                            username.set(String::new());
                            success.set(format!("Account {} created. They can now sign in.", user.username));
                            accounts.refetch();
                        }
                        Err(failure) => {
                            if auth::is_auth_error(&failure) {
                                auth.expire();
                            } else {
                                error.set(friendly_error(&failure));
                            }
                        }
                    }
                });
            }>
                <input type="hidden" name="expected_user_id" value=owner.get_value()/>
                <div class="mh-field">
                    <label for="new-account-username" class="mh-field__label">"Username"</label>
                    <p id="new-account-username-help" class="mh-field__help">"3–32 letters, numbers, or underscores."</p>
                    <input id="new-account-username" name="username" type="text" autocomplete="off" autocapitalize="none" spellcheck="false" required minlength="3" maxlength="32" class="mh-input mh-focusable" aria-describedby="new-account-username-help" disabled=move || busy.get() prop:value=move || username.get() on:input=move |event| username.set(event_target_value(&event))/>
                </div>
                <div class="mh-field">
                    <label for="new-account-password" class="mh-field__label">"Password"</label>
                    <p id="new-account-password-help" class="mh-field__help">"15–128 characters. Share it privately with the account holder."</p>
                    <input id="new-account-password" name="password" type="password" autocomplete="new-password" required minlength="15" maxlength="256" class="mh-input mh-focusable" aria-describedby="new-account-password-help" disabled=move || busy.get() prop:value=move || password.get() on:input=move |event| password.set(event_target_value(&event))/>
                </div>
                <div class="mh-field">
                    <label for="new-account-confirmation" class="mh-field__label">"Confirm password"</label>
                    <input id="new-account-confirmation" type="password" autocomplete="new-password" required minlength="15" maxlength="256" class="mh-input mh-focusable" disabled=move || busy.get() prop:value=move || confirmation.get() on:input=move |event| confirmation.set(event_target_value(&event))/>
                </div>
                {move || (!error.get().is_empty()).then(|| view! {<p class="mh-field__error" role="alert">{move || error.get()}</p>})}
                {move || (!success.get().is_empty()).then(|| view! {<p role="status">{move || success.get()}</p>})}
                <div class="mh-builder__actions">
                    <button type="submit" class="mh-btn mh-btn--primary mh-focusable" disabled=move || busy.get()>{move || if busy.get() {"Creating account…"} else {"Create account"}}</button>
                </div>
            </form>
            <h2 class="mh-accounts__heading">"Existing accounts"</h2>
            <Suspense fallback=|| view! {<p role="status">"Loading accounts…"</p>}>
                {move || match accounts.get() {
                    Some(Ok(users)) => view! {
                        <ul class="mh-accounts__list">
                            {users.into_iter().map(|user| view! {
                                <li><span>{user.username}</span><span>{if user.is_admin {"Administrator"} else {"Member"}}</span></li>
                            }).collect_view()}
                        </ul>
                    }.into_any(),
                    Some(Err(failure)) => view! {<LoadError message=friendly_error(&failure) retry=Callback::new(move |_| accounts.refetch())/>}.into_any(),
                    None => ().into_any(),
                }}
            </Suspense>
        </section>
    }
}
