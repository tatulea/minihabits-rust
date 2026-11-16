use super::*;
use crate::{
    domain::Tier,
    server::{TodaySystem, get_today, set_archived, set_check_in},
};

#[component]
pub fn Today() -> impl IntoView {
    let refresh = expect_context::<Refresh>().0;
    let owner_id = expect_context::<auth::Auth>().owner_id();
    let data = Resource::new(move || refresh.get(), move |_| get_today(owner_id.clone()));
    #[cfg(feature = "hydrate")]
    {
        let focus = window_event_listener(leptos::ev::focus, move |_| data.refetch());
        on_cleanup(move || focus.remove());
        // Read the resource in its reactive owner, not in the timer callback.
        let calendar = RwSignal::new(None);
        Effect::new(move |_| {
            calendar.set(
                data.get()
                    .and_then(Result::ok)
                    .map(|page| (page.time_zone, page.today)),
            );
        });
        if let Ok(timer) = set_interval_with_handle(
            move || {
                let has_new_day =
                    calendar
                        .get_untracked()
                        .is_some_and(|(time_zone, displayed_day)| {
                            crate::domain::today_in_timezone(&time_zone)
                                .is_ok_and(|today| today != displayed_day)
                        });
                if has_new_day {
                    data.refetch();
                }
            },
            std::time::Duration::from_secs(30),
        ) {
            on_cleanup(move || timer.clear());
        }
    }
    view! {
        <section class="mh-today">
            <header class="mh-today__head">
                <p class="mh-today__eyebrow">
                    <Suspense fallback=|| "Today">{move || data.get().and_then(Result::ok).map(|d| d.today.format("%A, %B %-d").to_string())}</Suspense>
                </p>
                <h1 class="mh-today__title">"Design the day, then let it run."</h1>
                <p class="mh-today__lead">"Systems work with the patterns you already have. Each one has a floor you can always hit. "<b>"Showing up counts."</b></p>
            </header>
            <Suspense fallback=|| view! {<p class="mh-today__hint">"Loading your systems…"</p>}>
                {move || data.get().map(|result| match result {
                    Err(error) => view! {<LoadError message=friendly_error(&error) retry=Callback::new(move |_| data.refetch())/>}.into_any(),
                    Ok(data) => view! {
                        {if data.systems.is_empty() {
                            view! {<div class="mh-today__empty">
                                <h2 class="mh-today__empty-title">"No systems yet."</h2>
                                <p class="mh-today__empty-lead">"Start with one small thing you already do. Name who you are becoming, not the chore."</p>
                                <a href="/app/design" class="mh-btn mh-btn--primary mh-focusable">"Add a system"</a>
                            </div>}.into_any()
                        } else {
                            view! {
                                <p class="mh-today__sec">"Your systems"</p>
                                <ul class="mh-systems">{data.systems.into_iter().map(|card| view! {<SystemCard card/>}).collect_view()}</ul>
                                <div class="mh-today__add"><a href="/app/design" class="mh-btn mh-btn--ghost mh-focusable">"Add a system"</a></div>
                            }.into_any()
                        }}
                        {(!data.archived.is_empty()).then(|| view! {
                            <details class="mh-archived">
                                <summary class="mh-focusable">"Archived systems ("{data.archived.len()}")"</summary>
                                <ul>{data.archived.into_iter().map(|system| view! {<ArchivedSystem id=system.id name=system.name/>}).collect_view()}</ul>
                            </details>
                        })}
                        <p class="mh-local-note">"Stored on this server · "{data.time_zone}" · Private to your account"</p>
                    }.into_any(),
                })}
            </Suspense>
        </section>
    }
}

#[component]
fn SystemCard(card: TodaySystem) -> impl IntoView {
    let refresh = expect_context::<Refresh>().0;
    let notice = expect_context::<Notice>().0;
    let busy = RwSignal::new(false);
    let tasks = ViewTasks::new();
    let auth = expect_context::<auth::Auth>();
    let session_revision = auth.revision.get_untracked();
    let error = RwSignal::new(String::new());
    let state = RwSignal::new(card.today_state);
    let id = StoredValue::new(card.system.id.clone());
    let expected_day = card.day;
    let log = Callback::new(move |tier: Tier| {
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        error.set(String::new());
        let desired = if matches!(
            (state.get_untracked(), tier),
            (DayState::Floor, Tier::Floor) | (DayState::Lift, Tier::Lift)
        ) {
            None
        } else {
            Some(tier)
        };
        let system_id = id.get_value();
        tasks.spawn(move |active| async move {
            let result = set_check_in(system_id, desired, expected_day).await;
            if auth.revision.get_untracked() != session_revision {
                return;
            }
            match result {
                Ok(()) => {
                    if active.is_active() {
                        state.set(desired.map(DayState::from).unwrap_or(DayState::Miss));
                    }
                    notice.set(
                        match desired {
                            Some(Tier::Floor) => "You showed up. That counts.",
                            Some(Tier::Lift) => "Went full. Nicely done.",
                            None => "Check-in undone.",
                        }
                        .into(),
                    );
                    refresh.update(|value| *value += 1);
                }
                Err(failure) => {
                    let message = friendly_error(&failure);
                    if auth::is_auth_error(&failure) {
                        notice.set("Your session ended. Sign in again to continue.".into());
                        auth.expire();
                        return;
                    } else if message.contains("day changed") {
                        notice.set(message);
                        refresh.update(|value| *value += 1);
                    } else if active.is_active() {
                        error.set(message);
                    } else {
                        notice.set(message);
                    }
                }
            }
            if active.is_active() {
                busy.set(false);
            }
        });
    });
    let resilience = card.momentum.resilience.round() as u32;
    let label = format!(
        "{}: resilience {} percent over {} days",
        card.system.name, resilience, card.momentum.window_days
    );
    let edit_href = format!("/app/systems/{}/edit", card.system.id);
    let edit_label = format!("Edit {}", card.system.name);
    view! {
        <li class="mh-card mh-sys" data-state=move || state_name(state.get()) aria-busy=move || busy.get().to_string()>
            <a href=edit_href class="mh-sys__tap mh-focusable" aria-label=edit_label>
                <span class="mh-sys__name">{card.system.name}
                    <span class="mh-sys__chev"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6"/></svg></span>
                </span>
                <span class="mh-sys__anchor">
                    <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="8"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3"/></svg>
                    <span>"Runs "<b>{card.system.anchor}</b></span>
                </span>
            </a>
            <div class="mh-sys__log">
                <fieldset class="mh-flc" aria-label="Log today">
                    <button type="button" class="mh-tier mh-tier--floor mh-focusable" aria-pressed=move || (state.get() != DayState::Miss).to_string() disabled=move || busy.get() on:click=move |_| log.run(Tier::Floor)>
                        <span class="mh-tier__k"><span class="mh-tier__ic"><CheckIcon/></span>"Show up"</span>
                        <span class="mh-tier__d">{card.system.floor}</span>
                    </button>
                    <button type="button" class="mh-tier mh-tier--lift mh-focusable" aria-pressed=move || (state.get() == DayState::Lift).to_string() disabled=move || busy.get() on:click=move |_| log.run(Tier::Lift)>
                        <span class="mh-tier__k"><span class="mh-tier__ic"><CheckIcon/></span>"Go full"</span>
                        <span class="mh-tier__d">{card.system.lift}</span>
                    </button>
                </fieldset>
                {move || (!error.get().is_empty()).then(|| view! {<p class="mh-field__error" role="alert">{move || error.get()}</p>})}
            </div>
            <div class="mh-sys__mom">
                <MomentumStrip series=card.momentum.series label/>
                <div class="mh-sys__mom-foot mh-tnum">
                    <span><b>{resilience}"%"</b>" resilience"</span>
                    <span class="mh-sys__split"><span class="mh-sys__split-lift">{card.momentum.split.lift}" full"</span>" · "<span class="mh-sys__split-floor">{card.momentum.split.floor}" showed up"</span></span>
                </div>
            </div>
        </li>
    }
}
#[component]
fn CheckIcon() -> impl IntoView {
    view! {<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 12l5 5L20 6"/></svg>}
}

#[component]
fn ArchivedSystem(id: String, name: String) -> impl IntoView {
    let id = StoredValue::new(id);
    let busy = RwSignal::new(false);
    let tasks = ViewTasks::new();
    let auth = expect_context::<auth::Auth>();
    let session_revision = auth.revision.get_untracked();
    let error = RwSignal::new(String::new());
    let refresh = expect_context::<Refresh>().0;
    let notice = expect_context::<Notice>().0;
    view! {
        <li><span>{name}</span><button type="button" class="mh-btn mh-btn--ghost mh-focusable" disabled=move || busy.get() on:click=move |_| {
            if busy.get_untracked() { return; }
            busy.set(true);
            let system_id = id.get_value();
            tasks.spawn(move |active| async move {
                let result = set_archived(system_id, false).await;
                        if auth.revision.get_untracked() != session_revision { return; }
                        match result {
                    Ok(()) => {notice.set("System restored.".into()); refresh.update(|value| *value += 1);}
                    Err(failure) => {
                        if auth::is_auth_error(&failure) {
                            notice.set("Your session ended. Sign in again to continue.".into());
                            auth.expire();
                                    return;
                        } else if active.is_active() {error.set(friendly_error(&failure));} else {notice.set(friendly_error(&failure));}
                    },
                }
                if active.is_active() {busy.set(false);}
            });
        }>"Restore"</button>{move || (!error.get().is_empty()).then(|| view! {<p role="alert">{move || error.get()}</p>})}</li>
    }
}
