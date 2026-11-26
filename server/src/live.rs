//! Live search: discover fresh URLs at query time, fetch the ones the
//! index doesn't have, and store them so ranking, snippets and research
//! briefs treat them like any other page.
//!
//! Discovery uses DuckDuckGo's HTML endpoint — keyless and pluggable via
//! the `VERIDEX_DISCOVER_URL` environment variable, which the integration
//! tests point at a local fixture. Fetched pages go through the same
//! extraction path as the crawler and obey `robots.txt` (agent
//! `web_weaver`, cached per origin).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use scraper::{Html, Selector};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use url::Url;
use veridex_core::{SqlitePool, db, extract};
use base64::Engine;

/// DuckDuckGo HTML search; the query goes in a form body (`q=...`).
const DEFAULT_DISCOVER_URL: &str = "https://html.duckduckgo.com/html/";

/// Bing HTML search; the query goes in a `?q=...` parameter.
const DEFAULT_FALLBACK_URL: &str = "https://www.bing.com/search";

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/126.0 Safari/537.36 VeridexBot/0.1";

/// Hard cap on pages fetched and stored per live query.
const MAX_FETCH: usize = 8;
/// Concurrent fetches within one query.
const MAX_CONCURRENT: usize = 4;
/// Whole-phase budget; callers wrap [`expand`] in this timeout so a slow
/// discovery or a stalled host can never hold a search hostage.
pub const BUDGET: Duration = Duration::from_secs(7);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(4);
const FETCH_TIMEOUT: Duration = Duration::from_secs(6);
const ROBOTS_TIMEOUT: Duration = Duration::from_secs(2);
/// Skip bodies larger than this (bytes).
const MAX_BODY: usize = 2 * 1024 * 1024;

/// Discovers candidate URLs for `query` and stores up to [`MAX_FETCH`] of
/// the ones the index doesn't have yet. Returns how many pages were added.
///
/// Never fails: discovery/fetch problems degrade to "0 new pages" and the
/// caller falls back to purely local results.
pub async fn expand(pool: &SqlitePool, query: &str, domain: Option<&str>) -> usize {
    let query = query.trim();
    if query.is_empty() {
        return 0;
    }

    // Let the discovery backend narrow to the requested host.
    let discovery_query = match domain.filter(|d| !d.trim().is_empty()) {
        Some(d) => format!("{query} site:{}", d.trim()),
        None => query.to_string(),
    };

    let hits = match discover(&discovery_query).await {
        Ok(hits) => hits,
        Err(err) => {
            tracing::warn!("live discovery failed: {err}");
            return 0;
        }
    };

    let candidates = filter_candidates(pool, hits).await;
    if candidates.is_empty() {
        return 0;
    }

    fetch_all(pool, candidates).await
}

/// Queries the discovery endpoint and extracts candidate URLs.
async fn discover(query: &str) -> Result<Vec<String>, String> {
    let endpoint = std::env::var("VERIDEX_DISCOVER_URL")
        .unwrap_or_else(|_| DEFAULT_DISCOVER_URL.to_string());
    let client = http_client(DISCOVER_TIMEOUT)?;

    // Primary: DuckDuckGo HTML (POST, keyless).
    if let Ok(hits) = discover_via_ddg(&client, &endpoint, query).await
        && !hits.is_empty()
    {
        return Ok(hits);
    }

    // Fallback: Bing HTML (GET, keyless).
    let fallback = std::env::var("VERIDEX_DISCOVER_FALLBACK_URL")
        .unwrap_or_else(|_| DEFAULT_FALLBACK_URL.to_string());
    discover_via_bing(&client, &fallback, query).await
}

async fn discover_via_ddg(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
) -> Result<Vec<String>, String> {
    let response = client
        .post(endpoint)
        .form(&[("q", query)])
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }
    let html = response.text().await.map_err(|e| format!("body: {e}"))?;
    Ok(parse_discovery(&html))
}

async fn discover_via_bing(
    client: &reqwest::Client,
    endpoint: &str,
    query: &str,
) -> Result<Vec<String>, String> {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let separator = if endpoint.contains('?') { '&' } else { '?' };
    let target = format!("{endpoint}{separator}q={encoded}");

    let response = client
        .get(&target)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status()));
    }
    let html = response.text().await.map_err(|e| format!("body: {e}"))?;
    Ok(parse_bing(&html))
}

/// Pulls target URLs out of a Bing HTML results page. Results live in
/// `li.b_algo` blocks; their anchors point through `/ck/a?u=a1<base64>`
/// redirects, so the real URL has to be base64-decoded.
fn parse_bing(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let block_selector = Selector::parse("li.b_algo").expect("valid selector");
    let blocks = doc.select(&block_selector);
    let mut out = Vec::new();
    for block in blocks {
        let anchor_selector = Selector::parse("h2 a").expect("valid selector");
        let anchor = block.select(&anchor_selector).next();
        if let Some(anchor) = anchor
            && let Some(href) = anchor.value().attr("href")
            && let Some(url) = bing_unwrap(href)
        {
            out.push(url);
        }
    }
    out
}

fn bing_unwrap(href: &str) -> Option<String> {
    let idx = href.find("u=a1")?;
    let token = href[idx + 4..].split('&').next()?.to_string();
    let padding = "=".repeat(token.len() % 4);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(format!("{token}{padding}"))
        .ok()?;
    String::from_utf8(decoded).ok()
}

fn http_client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// Pulls target URLs out of a DuckDuckGo HTML results page.
///
/// Result anchors wrap the real URL as `//duckduckgo.com/l/?uddg=<enc>`;
/// plain `http(s)` links are taken as-is.
pub fn parse_discovery(html: &str) -> Vec<String> {
    let doc = Html::parse_document(html);
    let selector = Selector::parse("a[href]").expect("valid CSS selector");

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for href in doc.select(&selector).filter_map(|a| a.value().attr("href")) {
        if let Some(url) = unwrap_result_link(href)
            && seen.insert(url.clone())
        {
            out.push(url);
        }
    }
    out
}

fn unwrap_result_link(href: &str) -> Option<String> {
    if let Some(query) = href
        .strip_prefix("//duckduckgo.com/l/?")
        .or_else(|| href.strip_prefix("https://duckduckgo.com/l/?"))
        .or_else(|| href.strip_prefix("http://duckduckgo.com/l/?"))
    {
        return url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned());
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    None
}

/// Validates a discovered hit: real web URL, no fragments, no
/// DuckDuckGo-internal pages.
fn normalize(hit: &str) -> Option<Url> {
    let mut url = Url::parse(hit).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    if host.is_empty() || host.contains("duckduckgo.com") {
        return None;
    }
    url.set_fragment(None);
    Some(url)
}

/// Keeps unique, valid URLs the index doesn't already have (max `MAX_FETCH`).
async fn filter_candidates(pool: &SqlitePool, hits: Vec<String>) -> Vec<Url> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    for hit in hits {
        let Some(url) = normalize(&hit) else { continue };
        if !seen.insert(url.to_string()) {
            continue;
        }
        match db::page_id_by_url(pool, url.as_str()).await {
            Ok(Some(_)) => continue, // already indexed
            Ok(None) => {}
            Err(err) => tracing::warn!("url lookup failed for {url}: {err}"),
        }
        out.push(url);
        if out.len() >= MAX_FETCH {
            break;
        }
    }
    out
}

/// Fetches candidates with bounded concurrency; returns how many pages
/// were newly stored.
async fn fetch_all(pool: &SqlitePool, candidates: Vec<Url>) -> usize {
    let client = match http_client(FETCH_TIMEOUT) {
        Ok(client) => client,
        Err(err) => {
            tracing::warn!("fetch client failed: {err}");
            return 0;
        }
    };
    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT));

    let mut set: JoinSet<bool> = JoinSet::new();
    for url in candidates {
        let pool = pool.clone();
        let client = client.clone();
        let semaphore = semaphore.clone();
        set.spawn(async move {
            let Ok(_permit) = semaphore.acquire_owned().await else {
                return false;
            };
            fetch_and_store(&pool, &client, url).await
        });
    }

    let mut stored = 0;
    while let Some(result) = set.join_next().await {
        if matches!(result, Ok(true)) {
            stored += 1;
        }
    }
    stored
}

/// Fetches one URL, extracts it with the shared pipeline and inserts it
/// into the index. Returns whether a new page was stored.
async fn fetch_and_store(pool: &SqlitePool, client: &reqwest::Client, url: Url) -> bool {
    if !robots_allowed(client, &url).await {
        tracing::debug!("robots.txt disallows {url}");
        return false;
    }

    let response = match client.get(url.clone()).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::debug!("fetch failed for {url}: {err}");
            return false;
        }
    };
    if !response.status().is_success() {
        return false;
    }

    // Only HTML is indexable (PDFs/images are skipped; the crawler handles
    // those on its own schedule).
    if let Some(content_type) = response.headers().get(reqwest::header::CONTENT_TYPE)
        && let Ok(content_type) = content_type.to_str()
    {
        let content_type = content_type.to_ascii_lowercase();
        if !content_type.contains("text/html") && !content_type.contains("application/xhtml") {
            return false;
        }
    }
    if response
        .content_length()
        .is_some_and(|len| len > MAX_BODY as u64)
    {
        return false;
    }

    let Ok(body) = response.text().await else {
        return false;
    };
    let body: String = if body.len() > MAX_BODY {
        body.chars().take(MAX_BODY).collect()
    } else {
        body
    };

    let page = extract::from_html(&body);
    if page.title.is_none() && page.content.trim().is_empty() {
        return false; // nothing searchable in this document
    }

    let inserted = db::insert_page(
        pool,
        url.as_str(),
        url.host_str(),
        page.title,
        page.description,
        page.keywords,
        Some(page.content),
        body,
    )
    .await;

    match inserted {
        Ok(()) => {
            tracing::info!("live: stored {url}");
            true
        }
        Err(err) if err.to_string().contains("UNIQUE constraint failed") => false,
        Err(err) => {
            tracing::warn!("live: failed to store {url}: {err}");
            false
        }
    }
}

fn robots_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `robots.txt` check for agent `web_weaver` (same as the crawler), cached
/// per origin for the process lifetime. A missing or unreachable
/// `robots.txt` means "allowed".
async fn robots_allowed(client: &reqwest::Client, url: &Url) -> bool {
    let origin = url.origin().ascii_serialization();

    let cached = robots_cache()
        .lock()
        .expect("robots cache lock poisoned")
        .get(&origin)
        .cloned();

    let robots_txt = match cached {
        Some(txt) => txt,
        None => {
            let txt = match client
                .get(format!("{origin}/robots.txt"))
                .timeout(ROBOTS_TIMEOUT)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    response.text().await.unwrap_or_default()
                }
                _ => String::new(),
            };
            robots_cache()
                .lock()
                .expect("robots cache lock poisoned")
                .insert(origin.clone(), txt.clone());
            txt
        }
    };

    robotstxt::DefaultMatcher::default().one_agent_allowed_by_robots(
        &robots_txt,
        "web_weaver",
        url.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wrapped_and_direct_result_links() {
        let html = r#"
            <html><body>
              <a rel="nofollow" class="result__a"
                 href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fplato.stanford.edu%2Fentries%2Fconsciousness%2F&amp;rut=abc">
                Consciousness
              </a>
              <a href="https://example.com/direct">Direct</a>
              <a href="/relative">Relative</a>
              <a href="mailto:hi@example.com">Mail</a>
            </body></html>"#;

        let hits = parse_discovery(html);
        assert_eq!(
            hits[0],
            "https://plato.stanford.edu/entries/consciousness/"
        );
        assert!(hits.contains(&"https://example.com/direct".to_string()));
        assert!(!hits.iter().any(|h| h.starts_with('/')));
        assert!(!hits.iter().any(|h| h.starts_with("mailto:")));
    }

    #[test]
    fn normalize_rejects_internal_and_non_web_urls() {
        assert!(normalize("https://duckduckgo.com/?q=x").is_none());
        assert!(normalize("javascript:alert(1)").is_none());
        assert!(normalize("/relative/path").is_none());

        let url = normalize("https://example.com/a#section").expect("valid");
        assert_eq!(url.as_str(), "https://example.com/a"); // fragment stripped
    }

    #[test]
    fn discovery_page_without_results_is_empty() {
        assert!(parse_discovery("<html><body><p>no results</p></body></html>").is_empty());
    }

    #[test]
    fn parses_bing_result_redirects() {
        let html = r#"<html><body><ul>
            <li class="b_algo">
              <h2><a href="https://www.bing.com/ck/a?!&amp;&amp;p=1&amp;u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9wYWdl">Bing Result</a></h2>
            </li>
            <li class="b_algo">
              <h2><a href="https://www.bing.com/ck/a?!&amp;&amp;p=2&amp;u=a1aHR0cHM6Ly9jb2RlLmNvbS9vdGhlcg==">Bing Result 2</a></h2>
            </li>
            <li class="b_algo">
              <h2><a href="https://www.bing.com/ck/a?!&amp;&amp;p=3">Bing Result 3</a></h2>
            </li>
        </body></html>"#;

        let hits = parse_bing(html);
        assert_eq!(hits.len(), 2, "expected two redirect-decoded hits: {hits:?}");
        assert_eq!(hits[0], "https://example.com/page");
        assert_eq!(hits[1], "https://code.com/other");
    }
}
