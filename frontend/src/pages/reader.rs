use leptos::prelude::*;

use crate::api;
use crate::components::{pretty_signal, GradeBadge};

#[component]
pub fn PageReader(id: i64) -> impl IntoView {
    let page = LocalResource::new(move || async move { api::page(id).await });

    view! {
        <section class="reader">
            <div class="reader-toolbar">
                <a class="btn ghost" href="#/">"← Search"</a>
            </div>
            {move || match page.get() {
                None => view! { <div class="loading">"Loading page…"</div> }.into_any(),
                Some(Err(err)) => view! {
                    <div class="error">{format!("Could not load page: {err}")}</div>
                }
                .into_any(),
                Some(Ok(page)) => render_page(page).into_any(),
            }}
        </section>
    }
}

fn render_page(page: api::PageResponse) -> impl IntoView {
    let title = page
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| page.url.clone());
    let signals = page.signals.clone();
    let description = page.description.clone().filter(|d| !d.trim().is_empty());
    let content = page
        .content
        .as_deref()
        .map(|c| normalize_content(c, &title))
        .filter(|c| !c.trim().is_empty())
        .unwrap_or_else(|| "(no stored content)".to_string());
    let truncated = page.truncated;

    view! {
        <article class="card page-card">
            <h1 class="page-title">{title}</h1>
            <a class="source-url" href=page.url.clone() target="_blank" rel="noreferrer">
                {page.url.clone()}
            </a>
            <div class="authority-line">
                <GradeBadge grade=page.grade.clone()/>
                <span>{format!("authority {:.2}", page.authority)}</span>
                <span class="dot">"·"</span>
                <span>{page.host.clone().unwrap_or_default()}</span>
                <span class="dot">"·"</span>
                <span>{format!("crawled {}", page.crawled_at)}</span>
            </div>
            {(!signals.is_empty()).then(|| {
                view! {
                    <div class="signals">
                        {signals.into_iter().map(|s| view! { <span class="signal">{pretty_signal(&s)}</span> }).collect_view()}
                    </div>
                }
            })}
            {description.map(|d| view! { <p class="page-description">{d}</p> })}
            <div class="reader-content">{content}</div>
            {truncated.then(|| {
                view! { <p class="truncated">"Content was truncated at 20,000 characters."</p> }
            })}
        </article>
    }
}

/// The crawler stores text hard-wrapped at ~60 columns. Reflow it into
/// readable paragraphs, keep bullets on their own lines, and drop the
/// leading copy of the page title.
fn normalize_content(raw: &str, title: &str) -> String {
    let title_lower = title.to_ascii_lowercase();
    let mut paragraphs: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut checked_first_line = false;

    let flush = |current: &mut Vec<&str>, paragraphs: &mut Vec<String>| {
        if !current.is_empty() {
            paragraphs.push(current.join(" "));
            current.clear();
        }
    };

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            flush(&mut current, &mut paragraphs);
            continue;
        }
        // Drop the document's own H1, which the crawler keeps verbatim
        // (it is already shown as the page title).
        if !checked_first_line {
            checked_first_line = true;
            let words = line.split_whitespace().count();
            if words <= 12
                && !line.ends_with(['.', '!', '?'])
                && title_lower.contains(&line.to_ascii_lowercase())
            {
                continue;
            }
        }
        if line.starts_with('-') || line.starts_with('*') || line.starts_with('•') {
            flush(&mut current, &mut paragraphs);
            paragraphs.push(line.to_string());
        } else {
            current.push(line);
        }
    }
    flush(&mut current, &mut paragraphs);

    let first_is_title = paragraphs.first().is_some_and(|first| {
        let first_lower = first.to_ascii_lowercase();
        first.split_whitespace().count() <= 12
            && !first.ends_with(['.', '!', '?'])
            && title_lower.contains(first_lower.trim())
    });
    if first_is_title {
        paragraphs.remove(0);
    }

    let mut out = String::new();
    for paragraph in &paragraphs {
        if out.is_empty() {
            out.push_str(paragraph);
        } else if paragraph.starts_with('-')
            || paragraph.starts_with('*')
            || paragraph.starts_with('•')
        {
            out.push('\n');
            out.push_str(paragraph);
        } else {
            out.push_str("\n\n");
            out.push_str(paragraph);
        }
    }
    out
}
