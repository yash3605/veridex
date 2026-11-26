//! SQLite storage layer shared by the crawler and the search server.
//!
//! Schema overview:
//! * `pages`     — one row per crawled page (metadata + extracted content)
//! * `pages_fts` — FTS5 external-content index over `pages` (porter-stemmed)
//! * `queue`     — persistent crawl frontier for resumable crawls

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{FromRow, Row, SqlitePool};
use std::str::FromStr;

/// Connect to (creating if missing) the SQLite database at `path`.
pub async fn connect(path: &str) -> Result<SqlitePool, sqlx::Error> {
    let options =
        SqliteConnectOptions::from_str(&format!("sqlite://{path}"))?.create_if_missing(true);
    SqlitePoolOptions::new().connect_with(options).await
}

/// Create every table/index the system needs. Safe to call repeatedly.
pub async fn init_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    create_table(pool).await?;
    create_fts_table(pool).await?;
    create_queue_table(pool).await?;
    ensure_host_column(pool).await?;
    Ok(())
}

pub async fn create_table(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            url TEXT NOT NULL UNIQUE,
            host TEXT,
            title TEXT,
            description TEXT,
            keywords TEXT,
            raw_html TEXT,
            content TEXT,
            crawled_at DATETIME DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pages_host ON pages(host)",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn create_fts_table(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // Porter stemming on top of unicode61 so "network" matches "networks"
    // and "analyzed" matches "analyse"/"analysis"-style variants.
    sqlx::query(
        "CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
            url, title, description, keywords, content,
            content='pages', content_rowid='id',
            tokenize='porter unicode61 remove_diacritics 2'
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS pages_ai AFTER INSERT ON pages BEGIN
        INSERT INTO pages_fts(rowid, url, title, description, keywords, content)
        VALUES (new.id, new.url, new.title, new.description, new.keywords, new.content);
    END;",
    )
    .execute(pool)
    .await?;

    Ok(())
}

/// Add the `host` column to databases created before it existed.
async fn ensure_host_column(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("PRAGMA table_info(pages)")
        .fetch_all(pool)
        .await?;
    let has_host = rows.iter().any(|r| r.get::<String, _>("name") == "host");
    if !has_host {
        sqlx::query("ALTER TABLE pages ADD COLUMN host TEXT")
            .execute(pool)
            .await?;
        // Backfill from stored URLs: strip scheme and everything after the host.
        sqlx::query(
            "UPDATE pages SET host = lower(
                CASE
                    WHEN instr(url, '://') > 0 THEN
                        substr(
                            url,
                            instr(url, '://') + 3,
                            CASE
                                WHEN instr(substr(url, instr(url, '://') + 3), '/') > 0 THEN
                                    instr(substr(url, instr(url, '://') + 3), '/') - 1
                                ELSE length(url)
                            END
                        )
                    ELSE url
                END
            ) WHERE host IS NULL",
        )
        .execute(pool)
        .await?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_pages_host ON pages(host)")
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, FromRow)]
pub struct PageRow {
    pub id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
    pub content: Option<String>,
    pub crawled_at: String,
}

/// One FTS5 match with its raw (weighted) BM25 score. Lower `score` is
/// better — SQLite's `bm25()` returns negative numbers.
#[derive(Debug, Clone, FromRow)]
pub struct FtsMatch {
    pub id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
    pub content: Option<String>,
    pub crawled_at: String,
    pub score: f64,
}

#[allow(clippy::too_many_arguments)]
pub async fn insert_page(
    pool: &SqlitePool,
    url: &str,
    host: Option<&str>,
    title: Option<String>,
    description: Option<String>,
    keywords: Option<String>,
    content: Option<String>,
    raw_html: String,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO pages (url, host, title, description, keywords, raw_html, content)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(url)
    .bind(host)
    .bind(title)
    .bind(description)
    .bind(keywords)
    .bind(raw_html)
    .bind(content)
    .execute(pool)
    .await?;

    Ok(())
}

const BM25_WEIGHTS: &str = "bm25(pages_fts, 0.5, 8.0, 3.0, 3.0, 1.0)";

/// Returns up to `limit` FTS5 matches for an already-built query string
/// (see [`crate::query::build_fts_query`]), best match first.
///
/// When `host_filter` is given, only pages whose host equals it or is a
/// subdomain of it are returned (`domain:example.com` style filtering).
pub async fn search_matches(
    pool: &SqlitePool,
    fts_query: &str,
    host_filter: Option<&str>,
    limit: i64,
) -> Result<Vec<FtsMatch>, sqlx::Error> {
    let sql = format!(
        "SELECT p.id, p.url, p.host, p.title, p.description, p.keywords,
                p.content, p.crawled_at, {BM25_WEIGHTS} AS score
         FROM pages p
         JOIN pages_fts ON pages_fts.rowid = p.id
         WHERE pages_fts MATCH ?1
           AND (?2 IS NULL OR p.host = ?2 OR p.host LIKE '%.' || ?2)
         ORDER BY score ASC
         LIMIT ?3"
    );
    sqlx::query_as::<_, FtsMatch>(&sql)
        .bind(fts_query)
        .bind(host_filter)
        .bind(limit)
        .fetch_all(pool)
        .await
}

/// Total number of FTS5 matches for a query (subject to the same host
/// filter as [`search_matches`]).
pub async fn count_matches(
    pool: &SqlitePool,
    fts_query: &str,
    host_filter: Option<&str>,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS n
         FROM pages p
         JOIN pages_fts ON pages_fts.rowid = p.id
         WHERE pages_fts MATCH ?1
           AND (?2 IS NULL OR p.host = ?2 OR p.host LIKE '%.' || ?2)",
    )
    .bind(fts_query)
    .bind(host_filter)
    .fetch_one(pool)
    .await?;
    Ok(row.get::<i64, _>("n"))
}

/// Fetch full page rows by id, preserving the order of `ids`.
pub async fn get_pages(pool: &SqlitePool, ids: &[i64]) -> Result<Vec<PageRow>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (1..=ids.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, url, host, title, description, keywords, content, crawled_at
         FROM pages WHERE id IN ({placeholders})"
    );
    let mut query = sqlx::query_as::<_, PageRow>(&sql);
    for id in ids {
        query = query.bind(id);
    }
    let mut rows = query.fetch_all(pool).await?;
    // Restore caller-provided order.
    rows.sort_by_key(|r| ids.iter().position(|i| *i == r.id).unwrap_or(usize::MAX));
    Ok(rows)
}

pub async fn get_page(pool: &SqlitePool, id: i64) -> Result<Option<PageRow>, sqlx::Error> {
    sqlx::query_as::<_, PageRow>(
        "SELECT id, url, host, title, description, keywords, content, crawled_at
         FROM pages WHERE id = ?1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// Returns the id of the page already stored under `url`, if any.
///
/// Used by live search to skip URLs the index already knows about.
pub async fn page_id_by_url(pool: &SqlitePool, url: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query("SELECT id FROM pages WHERE url = ?1 LIMIT 1")
        .bind(url)
        .fetch_optional(pool)
        .await
        .map(|row| row.map(|r| r.get::<i64, _>("id")))
}

#[derive(Debug, Clone, FromRow)]
pub struct HostCount {
    pub host: String,
    pub count: i64,
}

#[derive(Debug, Clone, FromRow)]
pub struct IndexStats {
    pub pages: i64,
    pub oldest_crawl: Option<String>,
    pub newest_crawl: Option<String>,
}

pub async fn index_stats(pool: &SqlitePool) -> Result<IndexStats, sqlx::Error> {
    let row = sqlx::query_as::<_, IndexStats>(
        "SELECT COUNT(*) AS pages,
                MIN(crawled_at) AS oldest_crawl,
                MAX(crawled_at) AS newest_crawl
         FROM pages",
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn top_hosts(pool: &SqlitePool, limit: i64) -> Result<Vec<HostCount>, sqlx::Error> {
    sqlx::query_as::<_, HostCount>(
        "SELECT host, COUNT(*) AS count FROM pages
         WHERE host IS NOT NULL
         GROUP BY host ORDER BY count DESC LIMIT ?1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

// ---------------------------------------------------------------------------
// CLI search (kept simple for the `web_weaver search` subcommand)
// ---------------------------------------------------------------------------

pub struct SearchResult {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub score: f64,
}

pub async fn search(
    pool: &SqlitePool,
    query: &str,
    page: u32,
) -> Result<Vec<SearchResult>, sqlx::Error> {
    let Some(fts) = crate::query::build_fts_query(query) else {
        return Ok(Vec::new());
    };
    let offset = (page * 10) as i64;
    let matches = search_matches(pool, &fts, None, offset + 10).await?;
    Ok(matches
        .into_iter()
        .skip(offset as usize)
        .map(|m| SearchResult {
            url: m.url,
            title: m.title,
            description: m.description,
            score: m.score,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Crawl queue (persistent frontier for resumable crawls)
// ---------------------------------------------------------------------------

pub async fn create_queue_table(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS queue (
            url TEXT NOT NULL,
            depth INTEGER NOT NULL DEFAULT 0
        )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn push_to_queue(pool: &SqlitePool, url: &str, depth: u32) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO queue (url, depth) VALUES (?1, ?2)")
        .bind(url)
        .bind(depth)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn pop_from_queue(pool: &SqlitePool, url: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM queue WHERE url = ?1")
        .bind(url)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn load_queue(pool: &SqlitePool) -> Result<Vec<(String, u32)>, sqlx::Error> {
    let rows = sqlx::query("SELECT url, depth FROM queue")
        .fetch_all(pool)
        .await?;

    let result = rows
        .iter()
        .map(|row| {
            let url: String = row.get("url");
            let depth: u32 = row.get("depth");
            (url, depth)
        })
        .collect();

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = connect(":memory:").await.expect("in-memory db");
        init_schema(&pool).await.expect("schema");
        pool
    }

    #[tokio::test]
    async fn insert_search_and_count() {
        let pool = test_pool().await;
        insert_page(
            &pool,
            "https://stanford.edu/x",
            Some("stanford.edu"),
            Some("Attention Is All You Need".into()),
            None,
            None,
            Some("The transformer architecture uses self attention mechanisms.".into()),
            "<html></html>".into(),
        )
        .await
        .unwrap();

        let q = build_query_for_test("attention transformer");
        let hits = search_matches(&pool, &q, None, 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].host.as_deref(), Some("stanford.edu"));
        assert!(hits[0].score.is_finite());
        assert_eq!(count_matches(&pool, &q, None).await.unwrap(), 1);

        // Host filter matches subdomains too.
        assert_eq!(
            search_matches(&pool, &q, Some("stanford.edu"), 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            search_matches(&pool, &q, Some("mit.edu"), 10)
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn stemming_matches_variants() {
        let pool = test_pool().await;
        insert_page(
            &pool,
            "https://arxiv.org/abs/1",
            Some("arxiv.org"),
            Some("Neural Networks".into()),
            None,
            None,
            Some("A network of neurons.".into()),
            "<html></html>".into(),
        )
        .await
        .unwrap();

        // "networks" in the query should stem-match "network" in the content.
        let q = build_query_for_test("network");
        assert_eq!(search_matches(&pool, &q, None, 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn get_pages_preserves_order() {
        let pool = test_pool().await;
        for i in 0..3 {
            insert_page(
                &pool,
                &format!("https://example.org/{i}"),
                Some("example.org"),
                Some(format!("Page {i}")),
                None,
                None,
                Some(format!("content {i}")),
                "<html></html>".into(),
            )
            .await
            .unwrap();
        }
        let pages = get_pages(&pool, &[3, 1, 2]).await.unwrap();
        let ids: Vec<i64> = pages.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![3, 1, 2]);
    }

    fn build_query_for_test(raw: &str) -> String {
        crate::query::build_fts_query(raw).expect("query")
    }
}
