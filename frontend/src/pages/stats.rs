use leptos::prelude::*;

use crate::api;

#[component]
pub fn StatsPage() -> impl IntoView {
    let stats = LocalResource::new(|| async { api::stats().await });
    let refresh = move |_| stats.refetch();

    view! {
        <section class="stats">
            <div class="results-head">
                <div>
                    <h1>"Index"</h1>
                    <p class="sub">"Corpus overview — what the crawler has stored locally."</p>
                </div>
                <button class="btn ghost" on:click=refresh>"Refresh"</button>
            </div>

            {move || match stats.get() {
                None => view! { <div class="loading">"Counting pages…"</div> }.into_any(),
                Some(Err(err)) => view! {
                    <div class="error">{format!("Stats failed: {err}")}</div>
                }
                .into_any(),
                Some(Ok(stats)) => render_stats(stats).into_any(),
            }}
        </section>
    }
}

fn render_stats(stats: api::StatsResponse) -> impl IntoView {
    let oldest = stats
        .oldest_crawl
        .clone()
        .unwrap_or_else(|| "—".to_string());
    let newest = stats
        .newest_crawl
        .clone()
        .unwrap_or_else(|| "—".to_string());
    let max = stats.top_hosts.first().map(|h| h.count).unwrap_or(1).max(1);

    view! {
        <div class="stat-cards">
            <div class="card stat">
                <span class="stat-value">{stats.pages.to_string()}</span>
                <span class="stat-label">"pages indexed"</span>
            </div>
            <div class="card stat">
                <span class="stat-value small">{oldest}</span>
                <span class="stat-label">"oldest crawl"</span>
            </div>
            <div class="card stat">
                <span class="stat-value small">{newest}</span>
                <span class="stat-label">"newest crawl"</span>
            </div>
        </div>

        <h2 class="section-title">"Top hosts"</h2>
        <div class="host-list">
            {stats.top_hosts.into_iter().map(|h| {
                let pct = (h.count as f64 / max as f64 * 100.0).max(2.0);
                view! {
                    <div class="host-row">
                        <span class="host-name">{h.host}</span>
                        <div class="host-bar-track">
                            <div class="host-bar" style=format!("width: {pct:.1}%")></div>
                        </div>
                        <span class="host-count">{h.count.to_string()}</span>
                    </div>
                }
            }).collect_view()}
        </div>
    }
}
