use super::*;
use crate::{
    domain::{ANCHOR_SUGGESTIONS, SystemDraft},
    server::{EditorData, get_editor, save_system, set_archived},
};
use leptos_router::hooks::{use_navigate, use_params_map};

#[component]
pub fn Builder() -> impl IntoView {
    let params = use_params_map();
    let owner_id = expect_context::<auth::Auth>().owner_id();
    let editor = Resource::new(
        move || params.with(|params| params.get("id")),
        move |id| get_editor(id, owner_id.clone()),
    );
    view! {
        <Suspense fallback=|| view! {<p class="mh-today__hint">"Loading your system…"</p>}>
            {move || editor.get().map(|result| match result {
                Ok(data) => view! {<BuilderForm data/>}.into_any(),
                Err(error) => view! {<LoadError message=friendly_error(&error) retry=Callback::new(move |_| editor.refetch())/>}.into_any(),
            })}
        </Suspense>
    }
}

#[component]
fn BuilderForm(data: EditorData) -> impl IntoView {
    let auth = expect_context::<auth::Auth>();
    let session_revision = auth.revision.get_untracked();
    let owner_id = StoredValue::new(auth.owner_id());
    let is_edit = data.system.is_some();
    let recovered = auth.builder_recovery.get_untracked().filter(|saved| {
        saved.owner_id == owner_id.get_value()
            && saved.is_edit == is_edit
            && (!is_edit || saved.system_id == data.id)
    });
    let recovered_draft = recovered.is_some();
    let system_id = recovered
        .as_ref()
        .map(|saved| saved.system_id.clone())
        .unwrap_or_else(|| data.id.clone());
    let draft = data
        .system
        .map(|s| SystemDraft {
            name: s.name,
            anchor: s.anchor,
            floor: s.floor,
            lift: s.lift,
        })
        .unwrap_or_default();
    let baseline = StoredValue::new(draft.clone());
    let draft = recovered.map(|saved| saved.draft).unwrap_or(draft);
    if recovered_draft {
        auth.builder_recovery.set(None);
    }
    let name = RwSignal::new(draft.name);
    let anchor = RwSignal::new(draft.anchor);
    let floor = RwSignal::new(draft.floor);
    let lift = RwSignal::new(draft.lift);
    let error = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let tasks = ViewTasks::new();
    let id = StoredValue::new(system_id);
    let draft_owner = owner_id.get_value();
    let draft_id = id.get_value();
    Effect::new(move |_| {
        let draft = SystemDraft {
            name: name.get(),
            anchor: anchor.get(),
            floor: floor.get(),
            lift: lift.get(),
        };
        if draft != baseline.get_value() {
            auth.builder_recovery.set(Some(auth::BuilderRecovery {
                owner_id: draft_owner.clone(),
                system_id: draft_id.clone(),
                is_edit,
                draft,
            }));
        } else {
            auth.builder_recovery.update(|saved| {
                if saved.as_ref().is_some_and(|saved| {
                    saved.owner_id == draft_owner && saved.system_id == draft_id
                }) {
                    *saved = None;
                }
            });
        }
    });
    let notice = expect_context::<Notice>().0;
    let refresh = expect_context::<Refresh>().0;
    let navigate = use_navigate();
    let archive_navigate = navigate.clone();
    // Bound the large generated form's type to keep SSR stack frames small.
    view! {
        <section class="mh-builder">
            <header class="mh-today__head">
                <p class="mh-today__eyebrow">{if is_edit {"Edit a system"} else {"Design a system"}}</p>
                <h1 class="mh-today__title">{if is_edit {"Adjust how this one runs."} else {"Name who you are becoming."}}</h1>
                <p class="mh-today__lead">"A system rides a routine you already have. Give it a floor you can always hit, then a lift for the good days. "<b>"Showing up counts."</b></p>
            </header>
            {recovered_draft.then(|| view! {<p class="mh-auth__recovery" role="status">"Your unsaved system was recovered. Review it, then save when you are ready."</p>})}
            <div class="mh-builder__grid">
                <form class="mh-builder__form" aria-label="System builder" novalidate on:submit=move |event| {
                    event.prevent_default();
                    if busy.get_untracked() {return;}
                    let draft = SystemDraft {name:name.get_untracked(),anchor:anchor.get_untracked(),floor:floor.get_untracked(),lift:lift.get_untracked()};
                    if let Err(failure) = draft.validate() {error.set(failure.to_string()); return;}
                    error.set(String::new());
                    busy.set(true);
                    let navigate = navigate.clone();
                    let system_id = id.get_value();
                    let expected_owner = owner_id.get_value();
                    let recovery = auth::BuilderRecovery {owner_id: expected_owner.clone(), system_id: system_id.clone(), is_edit, draft: draft.clone()};
                    tasks.spawn(move |active| async move {
                        let result = save_system(system_id, draft, expected_owner).await;
                        if auth.revision.get_untracked() != session_revision { return; }
                        match result {
                            Ok(()) => {auth.clear_builder_recovery(&recovery); notice.set(if is_edit {"Changes saved."} else {"System added. You can show up today."}.into()); refresh.update(|value| *value += 1); if active.is_active() {navigate("/app", Default::default());} return;}
                            Err(failure) => {
                                if auth::is_auth_error(&failure) {
                                    notice.set("Your session ended. Sign in again, then return to this editor to recover your unsaved system.".into());
                                    auth.expire();
                                    return;
                                } else if active.is_active() {error.set(friendly_error(&failure));} else {notice.set(friendly_error(&failure));}
                            },
                        }
                        if active.is_active() {busy.set(false);}
                    });
                }>
                    <TextField id="system-name" label="Who are you becoming?" value=name busy placeholder="Become a reader"/>
                    <fieldset class="mh-builder__anchors">
                        <legend class="mh-field__label">"Where does it ride along?"</legend>
                        <div class="mh-builder__chips">
                            {ANCHOR_SUGGESTIONS.into_iter().map(|suggestion| view! {
                                <button type="button" class="mh-chip mh-focusable" disabled=move || busy.get() aria-pressed=move || anchor.get().trim().eq_ignore_ascii_case(suggestion).to_string() on:click=move |_| anchor.set(suggestion.into())>{suggestion}</button>
                            }).collect_view()}
                        </div>
                        <TextField id="system-anchor" label="Or describe your own" value=anchor busy placeholder="After morning coffee"/>
                    </fieldset>
                    <TextField id="system-floor" label="The floor (the smallest version that still counts)" value=floor busy placeholder="Read one page" help="Make it almost too easy. This is the one you can always hit."/>
                    <TextField id="system-lift" label="The lift (the fuller version, for good days)" value=lift busy placeholder="Read twenty pages"/>
                    {move || (!error.get().is_empty()).then(|| view! {<p class="mh-field__error" role="alert">{move || error.get()}</p>})}
                    <div class="mh-builder__actions">
                        <button type="submit" class="mh-btn mh-btn--primary mh-focusable" disabled=move || busy.get()>{move || if busy.get() {"Saving…"} else if is_edit {"Save changes"} else {"Add this system"}}</button>
                        <a href="/app" class="mh-btn mh-btn--ghost mh-focusable" on:click=move |_| {
                            let owner = owner_id.get_value();
                            let system_id = id.get_value();
                            auth.builder_recovery.update(|saved| {
                                if saved.as_ref().is_some_and(|saved| saved.owner_id == owner && saved.system_id == system_id) { *saved = None; }
                            });
                        }>"Cancel"</a>
                        {is_edit.then(|| view! {
                            <button type="button" class="mh-btn mh-btn--ghost mh-focusable mh-builder__archive" disabled=move || busy.get() on:click=move |_| {
                                if busy.get_untracked() {return;}
                                busy.set(true);
                                error.set(String::new());
                                let navigate = archive_navigate.clone();
                                let system_id = id.get_value();
                                let recovery = auth::BuilderRecovery {owner_id: owner_id.get_value(), system_id: system_id.clone(), is_edit, draft: SystemDraft {name: name.get_untracked(), anchor: anchor.get_untracked(), floor: floor.get_untracked(), lift: lift.get_untracked()}};
                    tasks.spawn(move |active| async move {
                                    let result = set_archived(system_id, true).await;
                        if auth.revision.get_untracked() != session_revision { return; }
                        match result {
                                        Ok(()) => {auth.clear_builder_recovery(&recovery); notice.set("Archived. You can restore it from Today any time.".into()); refresh.update(|value| *value += 1); if active.is_active() {navigate("/app", Default::default());} return;}
                                        Err(failure) => {
                                if auth::is_auth_error(&failure) {
                                    notice.set("Your session ended. Sign in again, then return to this editor to recover your unsaved system.".into());
                                    auth.expire();
                                    return;
                                } else if active.is_active() {error.set(friendly_error(&failure));} else {notice.set(friendly_error(&failure));}
                            },
                                    }
                                    if active.is_active() {busy.set(false);}
                                });
                            }>"Archive"</button>
                        })}
                    </div>
                </form>
                <div class="mh-preview" aria-hidden="true">
                    <p class="mh-preview__label">"Preview"</p>
                    <div class="mh-card mh-sys" data-state="miss">
                        <div class="mh-sys__tap">
                            <span class="mh-sys__name">{move || nonempty(name.get(), "Your system")}</span>
                            {move || (!anchor.get().trim().is_empty()).then(|| view! {
                                <span class="mh-sys__anchor"><svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="8"/><path d="M12 2v3M12 19v3M2 12h3M19 12h3"/></svg><span>"Runs "<b>{move || anchor.get().trim().to_lowercase()}</b></span></span>
                            })}
                        </div>
                        <div class="mh-sys__log"><div class="mh-preview__tiers">
                            <div class="mh-preview__tier mh-preview__tier--floor"><span class="mh-preview__tier-k">"Show up"</span><span class="mh-preview__tier-d">{move || nonempty(floor.get(), "the small version")}</span></div>
                            <div class="mh-preview__tier mh-preview__tier--lift"><span class="mh-preview__tier-k">"Go full"</span><span class="mh-preview__tier-d">{move || nonempty(lift.get(), "the full version")}</span></div>
                        </div></div>
                        <div class="mh-sys__mom"><div class="mh-preview__strip">{(0..30).map(|_| view! {<i></i>}).collect_view()}</div><div class="mh-sys__mom-foot mh-tnum"><span>"A small start is enough."</span></div></div>
                    </div>
                </div>
            </div>
        </section>
    }.into_any()
}
fn nonempty(value: String, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.into()
    } else {
        value.trim().into()
    }
}

#[component]
fn TextField(
    id: &'static str,
    label: &'static str,
    value: RwSignal<String>,
    busy: RwSignal<bool>,
    placeholder: &'static str,
    #[prop(default = "")] help: &'static str,
) -> impl IntoView {
    let help_id = format!("{id}-help");
    view! {<div class="mh-field">
        <label class="mh-field__label" for=id>{label}</label>
        {(!help.is_empty()).then(|| view! {<p class="mh-field__help" id=help_id.clone()>{help}</p>})}
        <input id=id class="mh-input mh-focusable" type="text" disabled=move || busy.get() required maxlength="500" placeholder=placeholder aria-describedby=if help.is_empty() {None} else {Some(help_id)} prop:value=move || value.get() on:input=move |event| value.set(event_target_value(&event))/>
    </div>}
}
