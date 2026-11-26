# veridex-server

Axum HTTP API for scholarly-first search and extractive research briefs.
It reads the same SQLite index the crawler writes (`crawled.db` by default).

## Run

```sh
# API only
cargo run -p veridex-server -- --db crawled.db --host 127.0.0.1 --port 8080

# API + built frontend (after `cd frontend && trunk build --release`)
cargo run -p veridex-server -- --db crawled.db --port 8080 \
  --static-dir frontend/dist
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--db` | `crawled.db` | Path to the SQLite database shared with the crawler |
| `--host` | `127.0.0.1` | Bind address |
| `--port`, `-p` | `8080` | Bind port |
| `--static-dir` | — | Serve the built frontend (e.g. `frontend/dist`) at `/` |

Schema creation/migration runs on startup, so the server can point at an
empty file too (you just get no results).

## Endpoints

| Method | Path | Description |
| --- | --- | --- |
| `GET` | `/api` | Endpoint index (JSON) |
| `GET` | `/` | Endpoint index (JSON) — only when no `--static-dir`; otherwise `index.html` |
| `GET` | `/api/health` | Liveness probe |
| `GET` | `/api/search` | Hybrid-ranked search |
| `POST` | `/api/summarize` | Extractive brief (PDF bytes or JSON) |
| `GET` | `/api/summarize` | Same, GET-friendly, always PDF |
| `GET` | `/api/page/{id}` | Full page detail |
| `GET` | `/api/stats` | Index statistics |

With `--static-dir`, every other path falls through to static file serving
(the SPA uses hash routes, so no rewrite rules are needed).

All errors are JSON: `{"error": "..."}` with status `400`, `404` or `500`.

### `GET /api/search`

| Param | Default | Notes |
| --- | --- | --- |
| `q` | — | Free-text query; may embed `site:host` / `domain:host` |
| `page` | `0` | 0-based result page |
| `limit` | `10` | Clamped to `1..=50` |
| `domain` | — | Explicit host filter (wins over an embedded token) |
| `live` | `false` | Discover fresh URLs from the live web, fetch the new ones, then search the enlarged index |

```json
{
  "query": "consciousness knowledge",
  "page": 0,
  "limit": 10,
  "total": 2,
  "took_ms": 3,
  "results": [
    {
      "id": 4,
      "url": "https://plato.stanford.edu/entries/properties-emergent/",
      "host": "plato.stanford.edu",
      "title": "Emergent Properties (Stanford Encyclopedia of Philosophy)",
      "snippet": "…",
      "score": 0.734,
      "relevance": 0.607,
      "authority": 1.0,
      "grade": "A",
      "signals": ["trusted host", "DOI", "references section"],
      "crawled_at": "2026-11-25 14:00:52"
    }
  ]
}
```

Ranking is scholarly-first by design:

```
score = 0.55 · relevance + 0.40 · authority + 0.05 · title_coverage
```

* `relevance` is the FTS5 BM25 score squashed with `s / (s + 2)` into `[0, 1)`.
  The absolute squash keeps SQLite's `IDF ≈ 0` epsilon (terms matching most of
  the corpus) near zero instead of amplifying noise.
* `authority` comes from curated trusted/demoted host lists, TLD rules
  (`.edu`, `.gov`, `.ac.*`, …) and page signals (DOI, references section,
  inline citations, long-form text, PDF links). Grades are `A`–`D`.
* `title_coverage` rewards query terms appearing in the title.

### Live search (`GET /api/search?live=true`)

With `live=true`, before searching, the server discovers candidate URLs for
the query via the discovery backend, fetches the ones missing from the index
(up to 8, robots-checked, HTML-only, bounded time budget), stores them, and
then searches the enlarged index — so live results get exactly the same
hybrid ranking as crawled pages, and research briefs can use them too.

The response gains two fields:

| Field | Notes |
| --- | --- |
| `live` | Whether live discovery ran for this query |
| `live_fetched` | How many pages were fetched and stored during this query (0 when nothing new was found) |

`POST /api/summarize` accepts the same `"live": true` flag in its body
(default `false`).

Live discovery uses DuckDuckGo's HTML endpoint (`POST /html/?q=…`, keyless)
and automatically falls back to Bing's HTML endpoint if DuckDuckGo is
unreachable or rate-limits the request. Endpoints can be overridden with the
`VERIDEX_DISCOVER_URL` (DuckDuckGo) and `VERIDEX_DISCOVER_FALLBACK_URL` (Bing)
environment variables — the integration tests point these at local fixtures.

When discovery fails, the server degrades silently to local-only results.

### `POST /api/summarize`

Request body (`q` is accepted as an alias for `query`):

| Field | Default | Notes |
| --- | --- | --- |
| `query` | required | Same syntax as search, incl. `site:`/`domain:` |
| `max_pages` | `6` | Pages analyzed after authority ranking |
| `max_sentences` | `10` | Clamped to `3..=30` |
| `format` | `"pdf"` | `"pdf"` → PDF bytes, `"json"` → structured document |
| `title` | query | Optional brief title |

Example:

```sh
curl -s -X POST http://localhost:8080/api/summarize \
  -H 'content-type: application/json' \
  -d '{"query":"consciousness knowledge","format":"json","max_pages":5}'

# PDF download
curl -s -X POST http://localhost:8080/api/summarize \
  -H 'content-type: application/json' \
  -d '{"query":"consciousness knowledge","format":"pdf"}' \
  -o brief.pdf
```

`format: "json"` returns:

```json
{
  "title": "consciousness knowledge",
  "query": "consciousness knowledge",
  "generated_at": "2026-11-25 23:38",
  "stats": {
    "pages_analyzed": 2,
    "sentences_considered": 1109,
    "sentences_selected": 8
  },
  "findings": [
    { "text": "Some such philosophers try to explain knowledge …", "source": 2 }
  ],
  "sources": [
    {
      "index": 1,
      "page_id": 4,
      "url": "https://plato.stanford.edu/entries/properties-emergent/",
      "host": "plato.stanford.edu",
      "title": "Emergent Properties (Stanford Encyclopedia of Philosophy)",
      "grade": "A",
      "authority": 1.0,
      "crawled_at": "2026-11-25 14:00:52",
      "sentences": ["…"]
    }
  ]
}
```

`format: "pdf"` responds with `Content-Type: application/pdf` and a
`Content-Disposition` attachment filename (no JSON envelope).

#### How the summary is built

1. The query runs through the same search pipeline; the top `max_pages`
   authority-ranked pages are taken. `domain:` filters apply here too.
2. Each page is split into sentences (hard-wrapped crawler text is rejoined;
   blank lines/bullets break sentences; headings, section numbers like
   `4.4.1`, footnote anchors like `[46]` and the leading page title are
   dropped; decimals and abbreviations like `Sect.` never split).
3. Sentences are scored on query coverage, page salience, position, length
   quality and source authority.
4. Greedy MMR selects diverse, non-redundant sentences, with a per-page cap
   so one source cannot dominate.
5. Results are grouped per source; the PDF renderer lays them out with a
   masthead, key findings, source breakdown (grade/authority/URL) and
   references, with `page N of M` footers.

No network calls and no LLM: the summarizer is deterministic and local.

### `GET /api/page/{id}`

Returns the stored page plus its authority assessment. Content is truncated
at 20,000 characters unless `?full=true`; `truncated` reports whether that
happened.

### `GET /api/stats`

```json
{
  "pages": 13,
  "oldest_crawl": "2026-11-25 13:57:16",
  "newest_crawl": "2026-11-25 14:00:55",
  "top_hosts": [{ "host": "plato.stanford.edu", "count": 13 }]
}
```

## Development

```sh
cargo test -p veridex-server          # unit + integration tests
cargo clippy -p veridex-server --all-targets -- -D warnings
```

Integration tests (`tests/api.rs`) spin the router up in-process against an
in-memory database — no server or network needed.
