use std::collections::HashMap;

use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::api::{self, SummaryDoc};
use crate::components::{GradeBadge, LiveToggle};
use crate::route;

#[derive(Clone, Debug, PartialEq)]
enum PdfStatus {
    Idle,
    Busy,
    Failed(String),
}

#[component]
pub fn BriefPage(q: Option<String>) -> impl IntoView {
    let query = q.unwrap_or_default();
    let has_query = !query.trim().is_empty();

    let q_input = RwSignal::new(query.clone());
    let on_submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let q = q_input.get_untracked();
        route::go(&route::brief_url(&q));
    };

    // Live-web search is on by default; toggling re-runs the brief.
    let live = RwSignal::new(true);

    let query_for_fetch = query.clone();
    let doc = LocalResource::new(move || {
        let q = query_for_fetch.clone();
        let live = live.get();
        async move {
            if q.trim().is_empty() {
                return Err("empty query".to_string());
            }
            api::summary_json(&q, 6, 10, live).await
        }
    });

    let pdf_status = RwSignal::new(PdfStatus::Idle);
    let query_for_pdf = query.clone();
    let on_pdf = move |_| {
        if matches!(pdf_status.get_untracked(), PdfStatus::Busy) {
            return;
        }
        let q = query_for_pdf.clone();
        let live = live.get_untracked();
        pdf_status.set(PdfStatus::Busy);
        spawn_local(async move {
            match api::download_pdf(&q, live).await {
                Ok(()) => pdf_status.set(PdfStatus::Idle),
                Err(err) => pdf_status.set(PdfStatus::Failed(err)),
            }
        });
    };

    let description = if has_query {
        format!("Extractive summary for \u{201c}{query}\u{201d} — local, deterministic, no LLM.")
    } else {
        "Generate a brief from the local index.".to_string()
    };

    let search_href = format!("#{}", route::search_url(&query, 0, None));

    view! {
        <section class="brief">
            <div class="brief-head">
                <div>
                    <h1>"Research brief"</h1>
                    <p class="sub">{description}</p>
                </div>
                {has_query.then(|| {
                    view! {
                        <div class="brief-actions">
                            <LiveToggle value=live/>
                            <button
                                class="btn primary"
                                on:click=on_pdf
                                disabled=move || matches!(pdf_status.get(), PdfStatus::Busy)
                            >
                                {move || match pdf_status.get() {
                                    PdfStatus::Busy => "Preparing PDF…".to_string(),
                                    _ => "Download PDF".to_string(),
                                }}
                            </button>
                            <a class="btn ghost" href=search_href>"Back to results"</a>
                        </div>
                    }
                })}
            </div>

            {(!has_query).then(|| {
                view! {
                    <form class="searchbar" on:submit=on_submit>
                        <input
                            class="input grow"
                            type="search"
                            placeholder="What should the brief cover?"
                            prop:value=move || q_input.get()
                            on:input=move |ev| q_input.set(event_target_value(&ev))
                        />
                        <button class="btn primary" type="submit">"Generate"</button>
                        <LiveToggle value=live/>
                    </form>
                }
            })}

            {move || match pdf_status.get() {
                PdfStatus::Failed(err) => view! {
                    <div class="error">{format!("PDF export failed: {err}")}</div>
                }
                .into_any(),
                _ => ().into_any(),
            }}

            {move || {
                if !has_query {
                    return ().into_any();
                }
                match doc.get() {
                    None => view! {
                        <div class="loading">
                            {if live.get() {
                                "Discovering and reading sources…"
                            } else {
                                "Reading the top sources…"
                            }}
                        </div>
                    }
                    .into_any(),
                    Some(Err(err)) => view! {
                        <div class="error">{format!("Summary failed: {err}")}</div>
                    }
                    .into_any(),
                    Some(Ok(doc)) => render_doc(doc).into_any(),
                }
            }}
        </section>
    }
}

fn render_doc(doc: SummaryDoc) -> impl IntoView {
    let stats = doc.stats.clone();
    let generated = doc.generated_at.clone();
    let findings = doc.findings.clone();
    let sources = doc.sources.clone();

    let mut lookup: HashMap<usize, (i64, String)> = HashMap::new();
    for source in &sources {
        lookup.insert(source.index, (source.page_id, source.title.clone()));
    }

    view! {
        <div class="brief-meta">
            <span class="chip">"Generated " {generated}</span>
            <span class="chip">{format!("{} pages", stats.pages_analyzed)}</span>
            <span class="chip">{format!("{} sentences considered", stats.sentences_considered)}</span>
            <span class="chip">{format!("{} findings", stats.sentences_selected)}</span>
        </div>

        <h2 class="section-title">"Key findings"</h2>
        <ol class="findings">
            {findings.into_iter().map(|f| {
                let (page_id, source_title) = lookup
                    .get(&f.source)
                    .cloned()
                    .unwrap_or((0, "Unknown source".to_string()));
                view! {
                    <li class="card finding">
                        <p class="finding-text">{f.text}</p>
                        <div class="finding-meta">
                            <span class="cite">{format!("[{}]", f.source)}</span>
                            <a href=format!("#/page/{page_id}")>{source_title}</a>
                        </div>
                    </li>
                }
            }).collect_view()}
        </ol>

        <h2 class="section-title">"Sources"</h2>
        <div class="sources">
            {sources.into_iter().map(|s| {
                view! {
                    <article class="card source">
                        <div class="source-head">
                            <GradeBadge grade=s.grade.clone()/>
                            <span class="cite">{format!("[{}]", s.index)}</span>
                            <a class="source-title" href=format!("#/page/{}", s.page_id)>{s.title.clone()}</a>
                        </div>
                        <div class="authority-line">
                            <span>{format!("authority {:.2}", s.authority)}</span>
                            <span class="dot">"·"</span>
                            <span>{s.host.clone().unwrap_or_default()}</span>
                            <span class="dot">"·"</span>
                            <span>{format!("crawled {}", s.crawled_at)}</span>
                        </div>
                        <a class="source-url" href=s.url.clone() target="_blank" rel="noreferrer">
                            {s.url.clone()}
                        </a>
                        <ul class="source-points">
                            {s.sentences.clone().into_iter().map(|t| view! { <li>{t}</li> }).collect_view()}
                        </ul>
                    </article>
                }
            }).collect_view()}
        </div>
    }
}
