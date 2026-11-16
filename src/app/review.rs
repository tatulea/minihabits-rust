use super::*;
use crate::{
    domain::{Reflection, ReviewSuggestion},
    server::{ReviewData, get_weekly_review, save_reflection},
};

#[component]
pub fn Review() -> impl IntoView {
    let refresh = expect_context::<Refresh>().0;
    let owner_id = expect_context::<auth::Auth>().owner_id();
    let data = Resource::new(
        move || refresh.get(),
        move |_| get_weekly_review(owner_id.clone()),
    );
    view! {
        <section class="mh-review">
            <Suspense fallback=|| view! {<p class="mh-today__hint">"Pulling your week together…"</p>}>
                <div class="mh-review__recap">
                    {move || data.get().map(|result| match result {
                        Ok(data) => view! {<ReviewContent data/>}.into_any(),
                        Err(error) => view! {<LoadError message=friendly_error(&error) retry=Callback::new(move |_| data.refetch())/>}.into_any(),
                    })}
                </div>
                <div class="mh-review__reflection">
                    <For each={move || data.get().and_then(Result::ok).map(|page| page.reflection).into_iter().collect::<Vec<_>>()} key=|reflection| reflection.week_start let:reflection>
                        <div>
                            <p class="mh-today__sec">"Reflect"</p>
                            <ReflectionForm reflection/>
                        </div>
                    </For>
                </div>
            </Suspense>
        </section>
    }
}

#[component]
fn ReviewContent(data: ReviewData) -> impl IntoView {
    view! {
        <>
            <header class="mh-today__head">
                <p class="mh-today__eyebrow">"Week of "{data.review.week_start.to_string()}" to "{data.review.week_end.to_string()}</p>
                <h1 class="mh-today__title">"Look back, then nudge forward."</h1>
                <p class="mh-today__lead">"A short recap of how each system ran this week, a moment to reflect, and a few gentle adjustments to consider."</p>
            </header>
            <p class="mh-today__sec">"This week, by system"</p>
            {if data.review.systems.is_empty() {
                view! {<div class="mh-card"><p class="mh-detail__notes-stub">"No active systems to recap yet."</p></div>}.into_any()
            } else {
                view! {<ul class="mh-review__systems">{data.review.systems.into_iter().map(|system| {
                    let label = format!("{}: {} of {} days kept", system.name, system.kept, system.window_days);
                    view! {
                        <li class="mh-card mh-review__system">
                            <div class="mh-review__system-head"><span class="mh-review__system-name">{system.name}</span><span class="mh-tnum"><b>{system.resilience.round() as u32}"%"</b>" resilience"</span></div>
                            <MomentumStrip series=system.series label/>
                            <p class="mh-review__split mh-tnum">{system.split.lift}" full · "{system.split.floor}" showed up · "{system.split.miss}" missed"</p>
                        </li>
                    }
                }).collect_view()}</ul>}.into_any()
            }}
            {(!data.review.suggestions.is_empty()).then(|| view! {
                <p class="mh-today__sec">"A few things to consider"</p>
                <ul class="mh-review__suggestions">{data.review.suggestions.into_iter().map(|suggestion| view! {<SuggestionCard suggestion/>}).collect_view()}</ul>
            })}
        </>
    }
}

#[component]
fn SuggestionCard(suggestion: ReviewSuggestion) -> impl IntoView {
    let dismissed = RwSignal::new(false);
    let notice = expect_context::<Notice>().0;
    view! {
        <li class="mh-card mh-review__suggestion" hidden=move || dismissed.get()>
            <p class="mh-review__suggestion-head">{suggestion.head}</p>
            <p class="mh-review__suggestion-body">{suggestion.body}</p>
            <div class="mh-review__suggestion-acts">
                {if let Some(id) = suggestion.system_id {
                    view! {<a href=format!("/app/systems/{id}/edit") class="mh-btn mh-btn--primary mh-focusable">"Apply"</a>}.into_any()
                } else {
                    view! {<button type="button" class="mh-btn mh-btn--primary mh-focusable" on:click=move |_| {notice.set("Noted. Showing up counts.".into()); dismissed.set(true);}>"Apply"</button>}.into_any()
                }}
                <button type="button" class="mh-btn mh-btn--ghost mh-focusable" on:click=move |_| dismissed.set(true)>"Not now"</button>
            </div>
        </li>
    }
}

#[component]
fn ReflectionForm(reflection: Reflection) -> impl IntoView {
    let auth = expect_context::<auth::Auth>();
    let session_revision = auth.revision.get_untracked();
    let owner_id = StoredValue::new(auth.owner_id());
    let baseline = StoredValue::new(reflection.clone());
    let recovered = auth.reflection_recovery.get_untracked().filter(|saved| {
        saved.owner_id == owner_id.get_value()
            && saved.reflection.week_start == reflection.week_start
    });
    let recovered_draft = recovered.is_some();
    let reflection = recovered
        .map(|saved| saved.reflection)
        .unwrap_or(reflection);
    if recovered_draft {
        auth.reflection_recovery.set(None);
    }
    let worked = RwSignal::new(reflection.worked);
    let obstacles = RwSignal::new(reflection.obstacles);
    let adjustment = RwSignal::new(reflection.adjustment);
    let week_start = reflection.week_start;
    let refresh = expect_context::<Refresh>().0;
    let busy = RwSignal::new(false);
    let tasks = ViewTasks::new();
    let error = RwSignal::new(String::new());
    let notice = expect_context::<Notice>().0;
    let draft_owner = owner_id.get_value();
    Effect::new(move |_| {
        let reflection = Reflection {
            week_start,
            worked: worked.get(),
            obstacles: obstacles.get(),
            adjustment: adjustment.get(),
        };
        if reflection != baseline.get_value() {
            auth.reflection_recovery.set(Some(auth::ReflectionRecovery {
                owner_id: draft_owner.clone(),
                reflection,
            }));
        } else {
            auth.reflection_recovery.update(|saved| {
                if saved.as_ref().is_some_and(|saved| {
                    saved.owner_id == draft_owner && saved.reflection.week_start == week_start
                }) {
                    *saved = None;
                }
            });
        }
    });
    view! {
        <form class="mh-card mh-review__reflect" aria-label="Weekly reflection" on:submit=move |event| {
            event.prevent_default();
            if busy.get_untracked() {return;}
            busy.set(true); error.set(String::new());
            let reflection = Reflection {week_start, worked:worked.get_untracked(), obstacles:obstacles.get_untracked(), adjustment:adjustment.get_untracked()};
            let expected_owner = owner_id.get_value();
            let recovery = auth::ReflectionRecovery {owner_id: expected_owner.clone(), reflection: reflection.clone()};
            tasks.spawn(move |active| async move {
                let result = save_reflection(reflection, expected_owner).await;
                        if auth.revision.get_untracked() != session_revision { return; }
                        match result {
                    Ok(()) => {auth.clear_reflection_recovery(&recovery); if active.is_active() {baseline.set_value(recovery.reflection.clone());} notice.set("Reflection saved.".into()); if !active.is_active() {refresh.update(|value| *value += 1);}},
                    Err(failure) => {
                        if auth::is_auth_error(&failure) {
                            notice.set("Your session ended. Sign in again, then return to Weekly review to recover your unsaved reflection.".into());
                            auth.expire();
                                    return;
                        } else if active.is_active() {error.set(friendly_error(&failure));} else {notice.set(friendly_error(&failure));}
                    },
                }
                if active.is_active() {busy.set(false);}
            });
        }>
            {recovered_draft.then(|| view! {<p class="mh-auth__recovery" role="status">"Your unsaved reflection was recovered. Review it, then save when you are ready."</p>})}
            <ReflectionField id="r-worked" label="What worked?" value=worked busy/>
            <ReflectionField id="r-obstacles" label="What got in the way?" value=obstacles busy/>
            <ReflectionField id="r-adjustment" label="One adjustment for next week." value=adjustment busy/>
            {move || (!error.get().is_empty()).then(|| view! {<p class="mh-field__error" role="alert">{move || error.get()}</p>})}
            <div class="mh-review__actions"><button type="submit" class="mh-btn mh-btn--primary mh-focusable" disabled=move || busy.get()>{move || if busy.get() {"Saving…"} else {"Save reflection"}}</button></div>
        </form>
    }
}

#[component]
fn ReflectionField(
    id: &'static str,
    label: &'static str,
    value: RwSignal<String>,
    busy: RwSignal<bool>,
) -> impl IntoView {
    view! {
        <label class="mh-field__label" for=id>{label}</label>
        <textarea id=id class="mh-input mh-review__area mh-focusable" rows="2" maxlength="10000" disabled=move || busy.get() prop:value=move || value.get() on:input=move |event| value.set(event_target_value(&event))></textarea>
    }
}
