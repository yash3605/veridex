# Web Weaver

A small, single-binary web crawler and full-text search indexer written in Rust. Crawls pages, extracts metadata and text content, stores everything in SQLite with FTS5 full-text search, and provides a simple CLI to crawl and search.

## Features

- **Polite crawling** — respects `robots.txt` per-host (cached for the lifetime of the process), rate-limited via `governor`, concurrency-controlled via Tokio `Semaphore`
- **Full-text search** — stores pages in SQLite with an FTS5 virtual table and trigger-based auto-indexing; search with `bm25` ranking
- **Content extraction** — extracts title, meta keywords/description, and readable text content (h1–h5, p, article, main, section elements)
- **Retry with backoff** — failed HTTP requests retry up to 3 times with 1s / 2s / 4s exponential backoff
- **Queue persistence** — pending URLs are stored in a SQLite `queue` table; if the process is interrupted, pending work is loaded on next run
- **URL deduplication** — canonical URL keys (lowercased host, significant port, normalized path, sorted query params) prevent re-crawling the same page
- **CLI** — two subcommands: `crawl` and `search`

## Quick start

```bash
# Build (debug)
cargo build

# Crawl a site (depth 1: seed + one level of links)
cargo run -- crawl -u https://books.toscrape.com -d 1

# Search the index
cargo run -- search -q "mystery"

# Paginated search (10 results per page, 0-based)
cargo run -- search -q "fiction" -p 1
```

## CLI reference

### `crawl`

```
crawl [OPTIONS] --url <URL>...

Options:
  -u, --url <URL>                  Seed URL(s) to start crawling from
  -f, --seed-file <SEED_FILE>      File with one URL per line (# comments ignored)
  -c, --concurrent-parse <N>       Max concurrent fetch/parse tasks [default: 20]
  -r, --rate-limiter <N>           Global requests per second [default: 50]
  -d, --max-depth <N>              Maximum crawl depth from seed [default: 3]
```

At least one of `--url` or `--seed-file` is required.

On startup, any URLs remaining in the SQLite queue from a previous interrupted run are loaded first, followed by the seed URLs. Each discovered link is persisted to the queue before being dispatched.

### `search`

```
search --query <QUERY> [--page <N>]

Options:
  -q, --query <QUERY>  FTS5 query string
  -p, --page <N>       Page number (10 results per page, 0-based) [default: 0]
```

Outputs URL, title, and description for each match, ordered by `bm25` relevance.

## Architecture

### Module layout

```
src/
  main.rs         — CLI parsing, DB init, wires components together
  cli.rs          — clap derive definitions for crawl / search
  crawler.rs      — core crawl loop: frontier (VecDeque + Mutex),
                    visited set (HashSet + Mutex), robots.txt checks,
                    fetch with retry, HTML parsing, link extraction,
                    content extraction, DB insertion
  db.rs           — SQLite schema (pages, pages_fts, queue), CRUD
                    helpers, FTS5 search
  politeness.rs   — robots.txt cache (Mutex<HashMap>), per-host
                    rate limiter (governor keyed)
```

### Crawl loop

1. **Seed** — seed URLs are pushed to the in-memory frontier (`VecDeque`) and persisted to the SQLite `queue` table
2. **Dispatch** — on each iteration, a URL is popped from the frontier (and deleted from the `queue` table); a `JoinSet` task is spawned to fetch and parse it
3. **Robots check** — before spawning, the URL is checked against the cached `robots.txt` for its host; blocked URLs are skipped
4. **Fetch** — the spawned task acquires a semaphore permit, waits for the per-host rate limiter, then issues `reqwest::get` with up to 3 retries (1s/2s/4s backoff); 429 responses are handled with `Retry-After` header support
5. **Parse** — the HTML body is parsed with `scraper`; title, meta keywords, meta description, and readable text (h1–h5, p, article, main, section) are extracted
6. **Link discovery** — all `<a href>` links are resolved against the page base URL; only `http`/`https` URLs are enqueued (via `enqueue_url` which persists to both the in-memory frontier and the SQLite `queue` table)
7. **Store** — the page metadata, extracted content, and raw HTML are inserted into the `pages` table; the FTS5 trigger automatically populates `pages_fts`; UNIQUE constraint violations (duplicate URL) are silently skipped
8. **Continue** — once the frontier is empty and all spawned tasks have completed, the crawl ends

### Queue persistence

On startup, any rows in the `queue` table are loaded into the in-memory frontier. As URLs are discovered, they are inserted into both the `queue` table and the in-memory frontier. When a URL is popped for processing, it is deleted from the `queue` table. This means an interrupted crawl resumes from where it left off — only URLs that were in-flight (dispatched but not yet processed) at the time of interruption are not re-queued.

### URL canonicalization

The `canonical_keys` function produces a deduplication key for each URL:
- Host is lowercased
- Default ports (80, 443) are omitted; non-default ports are included
- Trailing slashes on non-root paths are removed
- Query parameters are sorted lexicographically

Two URLs that differ only in casing, default port, trailing slash, or query order are treated as the same page.

## Database schema

### `pages`

| Column       | Type    | Notes                              |
|-------------|---------|-------------------------------------|
| id          | INTEGER | Primary key, auto-increment         |
| url         | TEXT    | UNIQUE, NOT NULL                    |
| title       | TEXT    | Extracted from `<title>`            |
| description | TEXT    | From `<meta name="description">`    |
| keywords    | TEXT    | From `<meta name="keywords">`       |
| raw_html    | TEXT    | Full HTTP response body             |
| content     | TEXT    | Extracted readable text             |
| crawled_at  | DATETIME | Default: `CURRENT_TIMESTAMP`       |

### `pages_fts` (FTS5 virtual table)

Columns indexed: `url`, `title`, `description`, `keywords`, `content_text` (sourced from `pages.content`). An `AFTER INSERT` trigger on `pages` keeps the FTS index in sync.

### `queue`

| Column | Type    | Notes                    |
|--------|---------|--------------------------|
| url    | TEXT    | NOT NULL                 |
| depth  | INTEGER | Crawl depth from seed    |

## Dependencies

| Crate            | Purpose                        |
|------------------|--------------------------------|
| `tokio`          | Async runtime                  |
| `reqwest`        | HTTP client                    |
| `scraper`        | HTML parsing / CSS selectors   |
| `sqlx`           | SQLite driver                  |
| `clap`           | CLI argument parsing           |
| `governor`       | Rate limiting                  |
| `robotstxt`      | robots.txt matching            |
| `url`            | URL parsing and manipulation   |
| `tracing`        | Structured logging             |
| `anyhow`         | Error handling                 |

## Build requirements

- Rust toolchain (edition 2024; use a recent stable toolchain)
- On some platforms: `libsqlite3-dev` with FTS5 support (`sudo apt install libsqlite3-dev` on Debian/Ubuntu)

## Limitations

- **Single-process** — the frontier and visited set live in memory behind `Arc<Mutex<...>>`; no distributed crawling
- **No JavaScript rendering** — only static HTML is fetched and parsed
- **No sitemap ingestion** — discovery is purely link-following from seeds
- **No per-host rate limit tuning** — the rate limiter is global; per-host limits are not independently configurable
- **No content deduplication** — identical content at different URLs is stored separately (the UNIQUE constraint is on URL, not content)
- **Mutex poison** — if a task panics while holding a mutex lock, the lock is poisoned and subsequent `.expect()` calls will panic; acceptable for this scale but not production-hardened

## Troubleshooting

**FTS5 errors**: If creating the FTS5 virtual table fails, your system SQLite may not have FTS5 enabled. Install `libsqlite3-dev` with FTS5 support, or use a Rust SQLite build that bundles FTS.

**Network errors**: Failed HTTP requests are retried up to 3 times with backoff, then logged and skipped. Check your network connectivity and proxy configuration.

## Contributing

Contributions are welcome. This project is intended to be a useful, well-documented FOSS tool.

- **Issues** — Open a GitHub issue for bugs, feature requests, or questions.
- **Pull requests** — Fork the repository, make your changes, and open a PR. Keep changes focused and include a clear description.
- **Code style** — Run `cargo fmt` and `cargo clippy` before committing. The CI should pass without new warnings.
- **Tests** — The project has no test suite yet; adding tests alongside changes is appreciated.
- **Schema changes** — If you modify the database schema, update the schema documentation in this README.
- **Commit messages** — Use concise, descriptive messages. No strict convention beyond clarity.

## License

This project is free and open source software licensed under the MIT License. See the [LICENSE](LICENSE) file for details.
