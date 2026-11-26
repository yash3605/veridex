use crate::politeness::PolitenessPolicy;
use sqlx::SqlitePool;
use veridex_core::{db, extract};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};
use url::Url;

async fn enqueue_url(
    pool: &SqlitePool,
    url_to_fetch: &Arc<Mutex<VecDeque<(Url, u32)>>>,
    url: Url,
    depth: u32,
) {
    if let Err(e) = db::push_to_queue(pool, url.as_str(), depth).await {
        error!("Failed to persist URL to queue: {}", e);
    }
    url_to_fetch
        .lock()
        .expect("url frontier lock poisoned")
        .push_back((url, depth));
}

pub async fn fetch_and_parse(
    urls: Vec<Url>,
    visited_url: Arc<Mutex<HashSet<String>>>,
    url_to_fetch: Arc<Mutex<VecDeque<(Url, u32)>>>,
    politeness: Arc<PolitenessPolicy>,
    semaphore: Arc<Semaphore>,
    pool: SqlitePool,
    max_depth: u32,
) -> anyhow::Result<()> {
    for url in urls {
        enqueue_url(&pool, &url_to_fetch, url, 0).await;
    }

    let mut join_set: JoinSet<anyhow::Result<Option<(Url, String, u32)>>> = JoinSet::new();

    loop {
        let item = url_to_fetch
            .lock()
            .expect("url frontier lock poisoned")
            .pop_front();

        match item {
            Some((curr_url, depth)) => {
                if let Err(e) = db::pop_from_queue(&pool, curr_url.as_str()).await {
                    error!("Failed to remove URL from queue: {}", e);
                }

                let host = curr_url.host_str().unwrap_or("Unknown").to_string();
                let visited_url_clone = Arc::clone(&visited_url);

                if !politeness.is_allowed(&curr_url).await {
                    info!("Skipping URL: {curr_url}");
                    continue;
                }

                let semaphore = semaphore.clone();
                info!("Crawling: {}", curr_url);
                let polite_handle = politeness.clone();
                join_set.spawn(async move {
                    let ck = canonical_keys(&curr_url);
                    if visited_url_clone
                        .lock()
                        .expect("visited set lock poisoned")
                        .contains(&ck)
                    {
                        return Ok(None);
                    }

                    let _permit = semaphore.acquire_owned().await?;

                    polite_handle.polite_limiter.until_key_ready(&host).await;
                    let response = {
                        let mut attempt = 0;
                        let mut delay = Duration::from_secs(1);
                        loop {
                            match reqwest::get(curr_url.clone()).await {
                                Ok(r) => {
                                    if r.status().is_server_error() {
                                        attempt += 1;
                                        if attempt >= 3 {
                                            error!(
                                                "Server error after {} attempts: {}",
                                                attempt,
                                                r.status()
                                            );
                                            return Ok(None);
                                        }
                                        warn!(
                                            "Server error {} (attempt {}), retrying in {:?}",
                                            r.status(),
                                            attempt,
                                            delay
                                        );
                                        sleep(delay).await;
                                        delay *= 2;
                                        continue;
                                    }
                                    break r;
                                }
                                Err(e) => {
                                    attempt += 1;
                                    if attempt >= 3 {
                                        error!("Request failed after {} attempts: {}", attempt, e);
                                        return Ok(None);
                                    }
                                    warn!(
                                        "Request failed (attempt {}), retrying in {:?}: {}",
                                        attempt, delay, e
                                    );
                                    sleep(delay).await;
                                    delay *= 2;
                                }
                            }
                        }
                    };

                    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                        let wait = parse_retry_after(&response).unwrap_or(Duration::from_secs(5));
                        warn!("429 from {}, sleeping {:?}", curr_url, wait);
                        sleep(wait).await;
                        return Ok(None);
                    }

                    // Never index error pages (404s etc.) — they pollute the
                    // search index with boilerplate like "Document Not Found".
                    if !response.status().is_success() {
                        warn!("Skipping {} — status {}", curr_url, response.status());
                        return Ok(None);
                    }

                    let resp = response.text().await?;
                    Ok(Some((curr_url, resp, depth)))
                });
            }
            None => {
                if join_set.is_empty() {
                    break;
                }

                let pool = pool.clone();
                if let Some(result) = join_set.join_next().await {
                    match result {
                        Err(e) => error!("Task Error: {:?}", e),
                        Ok(Err(e)) => error!("Task error: {:?}", e),
                        Ok(Ok(None)) => {}
                        Ok(Ok(Some((url, resp, depth)))) => {
                            let extracted = extract::from_html(&resp);

                            let base =
                                format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));

                            for href in extracted.links {
                                let parsed_url = Url::parse(href.as_str());

                                match parsed_url {
                                    Err(_) => {
                                        if let Ok(base_url) = Url::parse(&base)
                                            && let Ok(comp_url) = base_url.join(href.as_str())
                                        {
                                            if !is_supported_scheme(&comp_url) {
                                                continue;
                                            }
                                            let ck = canonical_keys(&comp_url);
                                            if visited_url
                                                .lock()
                                                .expect("visited set lock poisoned")
                                                .contains(&ck)
                                            {
                                                continue;
                                            }
                                            if depth < max_depth {
                                                enqueue_url(
                                                    &pool,
                                                    &url_to_fetch,
                                                    comp_url,
                                                    depth + 1,
                                                )
                                                .await;
                                            }
                                        }
                                    }
                                    Ok(t) => {
                                        if !is_supported_scheme(&t) {
                                            continue;
                                        }
                                        let ck = canonical_keys(&t);
                                        if visited_url
                                            .lock()
                                            .expect("visited set lock poisoned")
                                            .contains(&ck)
                                        {
                                            continue;
                                        }
                                        if depth < max_depth {
                                            enqueue_url(&pool, &url_to_fetch, t, depth + 1).await;
                                        }
                                    }
                                };
                            }
                            let insert_page = db::insert_page(
                                &pool,
                                url.as_str(),
                                url.host_str(),
                                extracted.title,
                                extracted.description,
                                extracted.keywords,
                                Some(extracted.content),
                                resp,
                            )
                            .await;
                            if let Err(e) = insert_page {
                                if e.to_string().contains("UNIQUE constraint failed") {
                                    debug!("URL already in DB, skipping: {}", url);
                                } else {
                                    error!("Error inserting into DB: {}", e);
                                }
                            };
                            visited_url
                                .lock()
                                .expect("visited set lock poisoned")
                                .insert(canonical_keys(&url));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn is_supported_scheme(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

fn canonical_keys(url: &Url) -> String {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let port = url.port_or_known_default();
    let port_part = match (port, url.scheme()) {
        (Some(p), _) if ![80, 443].contains(&p) => format!(":{}", p),
        _ => String::new(),
    };

    let mut path = url.path().to_string();
    if path.ends_with('/') && path != "/" {
        path.pop();
    }
    if path.is_empty() {
        path = "/".to_string();
    }

    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    pairs.sort();

    let query = if pairs.is_empty() {
        String::new()
    } else {
        let q: Vec<String> = pairs
            .into_iter()
            .map(|(k, v)| format!("{}:{}", k, v))
            .collect();
        format!("?{}", q.join("&"))
    };

    format!("{}{}{}{}", host, port_part, path, query)
}

fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let header = response.headers().get("Retry-After")?.to_str().ok()?;
    header.parse::<u64>().ok().map(Duration::from_secs)
}
