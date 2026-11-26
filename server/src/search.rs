//! `GET /api/search` — the main search endpoint.

use crate::error::ApiError;
use crate::rank::{self, Scored};
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use veridex_core::{db, query};

#[derive(Debug, Deserialize)]
pub struct SearchParams {
    /// Free-text query. Supports an embedded `site:`/`domain:` prefix.
    pub q: Option<String>,
    /// 0-based page number.
    #[serde(default)]
    pub page: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Optional host filter, e.g. `stanford.edu`.
    pub domain: Option<String>,
    /// Discover fresh results on the live web and store them before searching.
    #[serde(default)]
    pub live: bool,
}

fn default_limit() -> usize {
    10
}

#[derive(Debug, Serialize)]
pub struct SearchResponse {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    pub page: usize,
    pub limit: usize,
    pub total: usize,
    pub took_ms: u128,
    /// Whether live discovery ran for this query.
    pub live: bool,
    /// Pages fetched and stored from the live web for this query.
    pub live_fetched: usize,
    pub results: Vec<SearchResultDto>,
}

#[derive(Debug, Serialize)]
pub struct SearchResultDto {
    pub id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: Option<String>,
    pub snippet: String,
    /// Hybrid score in `[0, 1]`.
    pub score: f64,
    /// Normalized BM25 relevance component.
    pub relevance: f64,
    /// Source authority component.
    pub authority: f64,
    pub grade: &'static str,
    pub signals: Vec<String>,
    pub crawled_at: String,
}

/// Splits an embedded `site:`/`domain:` token out of the raw query,
/// returning the cleaned query text and the normalized host filter.
pub fn extract_domain(raw: String, param: Option<String>) -> (String, Option<String>) {
    if let Some(d) = param.filter(|s| !s.trim().is_empty()) {
        return (raw, Some(crate::authority::normalize_domain_filter(&d)));
    }
    let mut domain: Option<String> = None;
    let kept: Vec<String> = raw
        .split_whitespace()
        .map(|t| t.to_string())
        .filter(|t| {
            let lower = t.to_ascii_lowercase();
            let rest = lower
                .strip_prefix("site:")
                .or_else(|| lower.strip_prefix("domain:"));
            match rest {
                Some(host) if !host.is_empty() => {
                    domain = Some(crate::authority::normalize_domain_filter(host));
                    false
                }
                _ => true,
            }
        })
        .collect();
    (kept.join(" "), domain)
}

/// Runs the full search pipeline: build query → FTS match → hybrid
/// re-rank → snippet → paginate.
pub async fn search(
    State(state): State<AppState>,
    Query(params): Query<SearchParams>,
) -> Result<Json<SearchResponse>, ApiError> {
    let started = Instant::now();
    let raw_q = params.q.unwrap_or_default();
    let limit = params.limit.clamp(1, 50);
    let (clean_q, domain) = extract_domain(raw_q.clone(), params.domain);

    let terms = query::query_terms(&clean_q);

    let Some(fts) = query::build_fts_query(&clean_q) else {
        // Nothing searchable (empty/punctuation-only query).
        return Ok(Json(SearchResponse {
            query: raw_q,
            domain,
            page: params.page,
            limit,
            total: 0,
            took_ms: started.elapsed().as_millis(),
            live: false,
            live_fetched: 0,
            results: Vec::new(),
        }));
    };

    // Live mode: discover fresh URLs, fetch the ones the index is missing
    // (bounded and robots-checked), then search the enlarged index so new
    // pages rank exactly like crawled ones. Budgeted so a slow discovery
    // or stalled host can't hold the request.
    let live = params.live;
    let live_fetched = if live {
        match tokio::time::timeout(
            crate::live::BUDGET,
            crate::live::expand(&state.pool, &clean_q, domain.as_deref()),
        )
        .await
        {
            Ok(fetched) => fetched,
            Err(_) => {
                tracing::warn!("live expansion exceeded budget for '{clean_q}'");
                0
            }
        }
    } else {
        0
    };

    let total = db::count_matches(&state.pool, &fts, domain.as_deref())
        .await
        .map_err(ApiError::internal)?;

    let offset = params.page.saturating_mul(limit);
    let results = if total == 0 || offset >= total as usize {
        Vec::new()
    } else {
        let candidates = db::search_matches(
            &state.pool,
            &fts,
            domain.as_deref(),
            rank::CANDIDATE_POOL,
        )
        .await
        .map_err(ApiError::internal)?;
        let ranked = rank::rerank(candidates, &terms);
        ranked
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|s| to_dto(s, &terms))
            .collect()
    };

    Ok(Json(SearchResponse {
        query: raw_q,
        domain,
        page: params.page,
        limit,
        total: total as usize,
        took_ms: started.elapsed().as_millis(),
        live,
        live_fetched,
        results,
    }))
}

pub fn to_dto(s: Scored, terms: &[String]) -> SearchResultDto {
    let snippet = if let Some(content) = s.matched.content.as_deref() {
        let snip = rank::make_snippet(content, terms, 240);
        if snip.chars().count() >= 40 {
            snip
        } else {
            fallback_snippet(&snip, s.matched.description.as_deref(), terms)
        }
    } else {
        fallback_snippet("", s.matched.description.as_deref(), terms)
    };

    SearchResultDto {
        id: s.matched.id,
        url: s.matched.url,
        host: s.matched.host,
        title: s.matched.title,
        snippet,
        score: round3(s.score),
        relevance: round3(s.relevance),
        authority: round3(s.authority),
        grade: s.grade,
        signals: s.signals,
        crawled_at: s.matched.crawled_at,
    }
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn fallback_snippet(primary: &str, description: Option<&str>, terms: &[String]) -> String {
    if let Some(desc) = description.filter(|d| !d.trim().is_empty()) {
        let from_desc = rank::make_snippet(desc, terms, 240);
        if from_desc.chars().count() > primary.chars().count() {
            return from_desc;
        }
    }
    primary.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_prefix_is_extracted() {
        let (q, d) = extract_domain("photosynthesis site:stanford.edu".into(), None);
        assert_eq!(q, "photosynthesis");
        assert_eq!(d.as_deref(), Some("stanford.edu"));
    }

    #[test]
    fn domain_param_wins() {
        let (q, d) = extract_domain("neural nets".into(), Some("MIT.EDU".into()));
        assert_eq!(q, "neural nets");
        assert_eq!(d.as_deref(), Some("mit.edu"));
    }

    #[test]
    fn plain_query_unchanged() {
        let (q, d) = extract_domain("climate change".into(), None);
        assert_eq!(q, "climate change");
        assert!(d.is_none());
    }

    #[test]
    fn domain_only_query_yields_empty_text() {
        let (q, d) = extract_domain("site:arxiv.org".into(), None);
        assert_eq!(q, "");
        assert_eq!(d.as_deref(), Some("arxiv.org"));
    }
}
