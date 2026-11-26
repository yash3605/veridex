//! Page detail, index statistics and health endpoints.

use crate::authority;
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use veridex_core::db;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub service: &'static str,
}

pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "veridex-server",
    })
}

#[derive(Debug, Deserialize)]
pub struct PageParams {
    /// Include the full content (default truncates at 20k characters).
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Serialize)]
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
    pub grade: &'static str,
    pub signals: Vec<String>,
}

pub async fn page(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(params): Query<PageParams>,
) -> Result<Json<PageResponse>, ApiError> {
    let page = db::get_page(&state.pool, id)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::NotFound(format!("no page with id {id}")))?;

    let auth = authority::score_page(
        &page.url,
        page.host.as_deref(),
        page.title.as_deref(),
        page.description.as_deref(),
        page.keywords.as_deref(),
        page.content.as_deref(),
    );

    const LIMIT: usize = 20_000;
    let (content, truncated) = match page.content {
        Some(c) if !params.full && c.chars().count() > LIMIT => (
            Some(c.chars().take(LIMIT).collect::<String>()),
            true,
        ),
        other => (other, false),
    };

    Ok(Json(PageResponse {
        id: page.id,
        url: page.url,
        host: page.host,
        title: page.title,
        description: page.description,
        keywords: page.keywords,
        crawled_at: page.crawled_at,
        content,
        truncated,
        authority: (auth.score * 1000.0).round() / 1000.0,
        grade: authority::grade(auth.score),
        signals: auth.signals,
    }))
}

#[derive(Debug, Serialize)]
pub struct HostCountDto {
    pub host: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub pages: i64,
    pub oldest_crawl: Option<String>,
    pub newest_crawl: Option<String>,
    pub top_hosts: Vec<HostCountDto>,
}

pub async fn stats(State(state): State<AppState>) -> Result<Json<StatsResponse>, ApiError> {
    let index = db::index_stats(&state.pool)
        .await
        .map_err(ApiError::internal)?;
    let hosts = db::top_hosts(&state.pool, 15)
        .await
        .map_err(ApiError::internal)?;

    Ok(Json(StatsResponse {
        pages: index.pages,
        oldest_crawl: index.oldest_crawl,
        newest_crawl: index.newest_crawl,
        top_hosts: hosts
            .into_iter()
            .map(|h| HostCountDto {
                host: h.host,
                count: h.count,
            })
            .collect(),
    }))
}
