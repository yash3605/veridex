use leptos::prelude::*;

use crate::api::{self, SearchResponse};
use crate::components::{pretty_signal, GradeBadge, LiveToggle};
use crate::route;

#[component]
pub fn SearchPage(q: Option<String>, page: usize, domain: Option<String>) -> impl IntoView {
    let query = q.clone().unwrap_or_default();
    let domain_value = domain.clone().unwrap_or_default();

    let q_input = RwSignal::new(query.clone());
    let domain_input = RwSignal::new(domain_value);
    // Live-web search is on by default; toggling re-runs the query.
    let live = RwSignal::new(true);

    let has_query = !query.trim().is_empty();
    let query_for_fetch = query.clone();
    let domain_for_fetch = domain.clone();

    let results = LocalResource::new(move || {
        let q = query_for_fetch.clone();
        let d = domain_for_fetch.clone();
        let live = live.get();
        async move { api::search(&q, page, d.as_deref(), live).await }
    });

    let on_submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let q = q_input.get_untracked();
        let d = domain_input.get_untracked();
        route::go(&route::search_url(&q, 0, Some(&d)));
    };

    view! {
        <section class="hero">
            <h1>"Find the scholarship, skip the noise"</h1>
            <p class="sub">
                "Ranked by source authority first — peer-reviewed, .edu/.gov and long-form sources outrank content farms."
            </p>
            <form class="searchbar" on:submit=on_submit>
                <input
                    class="input grow"
                    type="search"
                    placeholder="Search the local index — e.g. consciousness knowledge"
                    prop:value=move || q_input.get()
                    on:input=move |ev| q_input.set(event_target_value(&ev))
                />
                <input
                    class="input domain"
                    type="text"
                    placeholder="domain (optional)"
                    prop:value=move || domain_input.get()
                    on:input=move |ev| domain_input.set(event_target_value(&ev))
                />
                <button class="btn primary" type="submit">"Search"</button>
                <LiveToggle value=live/>
            </form>
        </section>

        <section class="results">
            {move || {
                if !has_query {
                    return view! {
                        <div class="empty">"Enter a query above to search the local index."</div>
                    }
                    .into_any();
                }
                match results.get() {
                    None => view! {
                        <div class="loading">
                            {if live.get() {
                                "Searching local index + live web…"
                            } else {
                                "Searching…"
                            }}
                        </div>
                    }
                    .into_any(),
                    Some(Err(err)) => view! {
                        <div class="error">{format!("Search failed: {err}")}</div>
                    }
                    .into_any(),
                    Some(Ok(resp)) => {
                        render_results(resp, query.clone(), domain.clone()).into_any()
                    }
                }
            }}
        </section>
    }
}

fn render_results(resp: SearchResponse, query: String, domain: Option<String>) -> impl IntoView {
    let total = resp.total;
    let page = resp.page;
    let limit = resp.limit.max(1);
    let total_pages = total.div_ceil(limit);
    let took_ms = resp.took_ms;
    let live_fetched = resp.live_fetched;
    let results = resp.results;

    let domain_note = domain
        .clone()
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!(" · domain {d}"))
        .unwrap_or_default();
    let brief_href = format!("#{}", route::brief_url(&query));

    let prev_query = query.clone();
    let prev_domain = domain.clone();
    let next_query = query;
    let next_domain = domain;
    let has_prev = page > 0 && total > 0;
    let has_next = (page + 1) * limit < total;

    view! {
        <div class="results-head">
            <div class="result-meta">
                <strong>{format!("{total} result{}", if total == 1 { "" } else { "s" })}</strong>
                <span class="dot">"·"</span>
                <span>{format!("{took_ms} ms")}</span>
                <span>{domain_note}</span>
                {(live_fetched > 0).then(|| {
                    view! {
                        <span class="chip chip-live">
                            {format!(
                                "+{live_fetched} new page{} from the live web",
                                if live_fetched == 1 { "" } else { "s" },
                            )}
                        </span>
                    }
                })}
            </div>
            <a class="btn ghost" href=brief_href>"Research brief →"</a>
        </div>

        <ol class="result-list">
            {results.into_iter().map(|r| view! { <ResultCard r=r/> }).collect_view()}
        </ol>

        {(total == 0).then(|| view! {
            <div class="empty">"No pages matched. Try broader terms or drop the domain filter."</div>
        })}

        {(total > limit).then(|| view! {
            <div class="pager">
                <button
                    class="btn"
                    disabled=!has_prev
                    on:click=move |_| {
                        if has_prev {
                            route::go(&route::search_url(&prev_query, page - 1, prev_domain.as_deref()));
                        }
                    }
                >
                    "← Previous"
                </button>
                <span class="page-num">"page "
                    {format!("{} of {}", page + 1, total_pages.max(1))}
                </span>
                <button
                    class="btn"
                    disabled=!has_next
                    on:click=move |_| {
                        if has_next {
                            route::go(&route::search_url(&next_query, page + 1, next_domain.as_deref()));
                        }
                    }
                >
                    "Next →"
                </button>
            </div>
        })}
    }
}

#[component]
fn ResultCard(r: api::SearchResultDto) -> impl IntoView {
    let href = format!("#/page/{}", r.id);
    let title = r
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| r.url.clone());
    let signals = r.signals.clone();
    let host = r.host.clone().filter(|h| !h.is_empty());

    view! {
        <li class="card result">
            <div class="result-head">
                <GradeBadge grade=r.grade.clone()/>
                <a class="result-title" href=href>
                    {title}
                </a>
            </div>
            <div class="result-url">{r.url.clone()}</div>
            <p class="snippet">{r.snippet.clone()}</p>
            <div class="chips">
                {host.map(|h| view! { <span class="chip chip-host">{h}</span> })}
                <span class="chip" title="Hybrid score">
                    {format!("score {:.3}", r.score)}
                </span>
                <span class="chip" title="BM25 relevance">
                    {format!("relevance {:.0}%", r.relevance * 100.0)}
                </span>
                <span class="chip" title="Source authority">
                    {format!("authority {:.2}", r.authority)}
                </span>
            </div>
            {(!signals.is_empty()).then(|| {
                view! {
                    <div class="signals">
                        {signals.into_iter().map(|s| view! { <span class="signal">{pretty_signal(&s)}</span> }).collect_view()}
                    </div>
                }
            })}
        </li>
    }
}
