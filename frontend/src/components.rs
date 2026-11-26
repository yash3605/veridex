//! Small shared view components.

use leptos::prelude::*;

/// Letter grade for the source-authority score (A–D).
#[component]
pub fn GradeBadge(grade: String) -> impl IntoView {
    let class = format!("badge grade-{}", grade.to_ascii_lowercase());
    view! { <span class=class title="Source authority grade">{grade}</span> }
}

/// Human-friendly rendering of a server signal key (`long_form` → `long form`).
pub fn pretty_signal(signal: &str) -> String {
    signal.replace('_', " ")
}

/// On/off switch for live-web search (discover + fetch fresh results).
#[component]
pub fn LiveToggle(value: RwSignal<bool>) -> impl IntoView {
    view! {
        <label
            class="live-toggle"
            title="Also search the live web: discovers fresh results, fetches the top ones and stores them locally"
        >
            <input
                type="checkbox"
                prop:checked=move || value.get()
                on:change=move |ev| value.set(event_target_checked(&ev))
            />
            <span class="switch"></span>
            <span class="live-label">"Live web"</span>
        </label>
    }
}
