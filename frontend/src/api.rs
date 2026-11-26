//! Typed client for the Veridex HTTP API.
//!
//! Requests use relative URLs (`/api/...`) by default, which works both with
//! the Trunk dev proxy and when the built app is served from the same origin
//! as the backend. To point at another origin, set `veridex_api` in
//! `localStorage` (e.g. `http://127.0.0.1:8080`).
//!
//! The DTOs mirror the server contract; not every field is rendered by the UI.

#![allow(dead_code)]

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub page: usize,
    pub limit: usize,
    pub total: usize,
    pub took_ms: u64,
    /// Whether live discovery ran for this query.
    #[serde(default)]
    pub live: bool,
    /// Pages fetched and stored from the live web for this query.
    #[serde(default)]
    pub live_fetched: usize,
    pub results: Vec<SearchResultDto>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SearchResultDto {
    pub id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: Option<String>,
    pub snippet: String,
    pub score: f64,
    pub relevance: f64,
    pub authority: f64,
    pub grade: String,
    pub signals: Vec<String>,
    pub crawled_at: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SummaryDoc {
    pub title: String,
    pub query: String,
    pub generated_at: String,
    pub stats: SummaryStats,
    pub findings: Vec<Finding>,
    pub sources: Vec<SourceSection>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SummaryStats {
    pub pages_analyzed: usize,
    pub sentences_considered: usize,
    pub sentences_selected: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Finding {
    pub text: String,
    pub source: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceSection {
    pub index: usize,
    pub page_id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: String,
    pub grade: String,
    pub authority: f64,
    pub crawled_at: String,
    pub sentences: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct StatsResponse {
    pub pages: i64,
    pub oldest_crawl: Option<String>,
    pub newest_crawl: Option<String>,
    pub top_hosts: Vec<HostCount>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct HostCount {
    pub host: String,
    pub count: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PageResponse {
    pub id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
    pub crawled_at: String,
    pub content: Option<String>,
    pub truncated: bool,
    pub authority: f64,
    pub grade: String,
    pub signals: Vec<String>,
}

#[derive(Deserialize)]
struct ApiErrorBody {
    error: String,
}

fn base() -> String {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("veridex_api").ok().flatten())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_default()
}

fn enc(s: &str) -> String {
    js_sys::encode_uri_component(s)
        .as_string()
        .unwrap_or_else(|| s.to_string())
}

async fn error_message(resp: gloo_net::http::Response) -> String {
    let status = resp.status();
    match resp.json::<ApiErrorBody>().await {
        Ok(body) => format!("{} (HTTP {status})", body.error),
        Err(_) => format!("HTTP {status}"),
    }
}

async fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, String> {
    let resp = gloo_net::http::Request::get(url)
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(error_message(resp).await);
    }
    resp.json::<T>()
        .await
        .map_err(|e| format!("bad response: {e}"))
}

pub async fn search(
    q: &str,
    page: usize,
    domain: Option<&str>,
    live: bool,
) -> Result<SearchResponse, String> {
    let mut url = format!(
        "{}api/search?q={}&page={}&limit=10&live={}",
        base(),
        enc(q.trim()),
        page,
        if live { "true" } else { "false" }
    );
    if let Some(domain) = domain.map(str::trim).filter(|d| !d.is_empty()) {
        url.push_str(&format!("&domain={}", enc(domain)));
    }
    get_json(&url).await
}

pub async fn summary_json(
    query: &str,
    max_pages: usize,
    max_sentences: usize,
    live: bool,
) -> Result<SummaryDoc, String> {
    let body = serde_json::json!({
        "query": query,
        "format": "json",
        "max_pages": max_pages,
        "max_sentences": max_sentences,
        "live": live,
    });
    let resp = gloo_net::http::Request::post(&format!("{}api/summarize", base()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .map_err(|e| format!("encode error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(error_message(resp).await);
    }
    resp.json::<SummaryDoc>()
        .await
        .map_err(|e| format!("bad response: {e}"))
}

pub async fn page(id: i64) -> Result<PageResponse, String> {
    get_json(&format!("{}api/page/{}?full=true", base(), id)).await
}

pub async fn stats() -> Result<StatsResponse, String> {
    get_json(&format!("{}api/stats", base())).await
}

/// Requests the PDF brief and hands it to the browser as a download.
pub async fn download_pdf(query: &str, live: bool) -> Result<(), String> {
    use wasm_bindgen::JsCast;

    let body = serde_json::json!({ "query": query, "format": "pdf", "live": live });
    let resp = gloo_net::http::Request::post(&format!("{}api/summarize", base()))
        .header("content-type", "application/json")
        .body(body.to_string())
        .map_err(|e| format!("encode error: {e}"))?
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;
    if !(200..300).contains(&resp.status()) {
        return Err(error_message(resp).await);
    }

    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read error: {e}"))?;

    let parts = js_sys::Array::new();
    parts.push(js_sys::Uint8Array::from(bytes.as_slice()).as_ref());
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)
        .map_err(|e| format!("blob error: {e:?}"))?;
    let object_url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|e| format!("url error: {e:?}"))?;

    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|e| format!("{e:?}"))?
        .dyn_into()
        .map_err(|_| "element is not an anchor".to_string())?;

    anchor.set_href(&object_url);
    anchor.set_download(&format!("veridex-brief-{}.pdf", slug(query)));

    if let Some(body_el) = document.body() {
        let _ = body_el.append_child(&anchor);
        anchor.click();
        let _ = body_el.remove_child(&anchor);
    } else {
        anchor.click();
    }

    // Give the browser time to start the download, then free the blob URL.
    let revoke = wasm_bindgen::closure::Closure::<dyn FnMut()>::new({
        let url = object_url.clone();
        move || {
            web_sys::Url::revoke_object_url(&url).ok();
        }
    });
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        revoke.as_ref().unchecked_ref(),
        10_000,
    );
    revoke.forget();

    Ok(())
}

fn slug(query: &str) -> String {
    let slug: String = query
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut out: String = slug.trim_matches('-').chars().take(48).collect();
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "summary".to_string()
    } else {
        out
    }
}
