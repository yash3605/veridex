# Veridex

A scholarly-first search engine: it ranks reliable, academic sources above
popular pages, and turns the top results into an extractive research brief
(PDF or JSON) without any LLM or network calls at query time.

Built for students and researchers who want signal, not clickbait. The
backend and the Rust/WebAssembly frontend are both complete.

## Workspace

```
veridex/
├── core/      veridex-core       shared SQLite + FTS5 index layer
├── crawler/   web_weaver         polite web crawler (writes the index)
├── server/    veridex-server     axum HTTP API (search, summarize, PDF)
└── frontend/  veridex-frontend   Leptos SPA compiled to WebAssembly
```

Both binaries share the same database (`crawled.db` by default), so crawl
first, then serve. The frontend is a standalone crate (excluded from the
backend workspace) built with [Trunk](https://trunkrs.dev).

## Quickstart

```sh
cargo build --release

# 1. Crawl a scholarly seed list (depth 1 keeps it polite/fast)
cargo run -p web_weaver -- --db crawled.db crawl -f crawler/seeds/scholarly.txt -d 1

# 2. Build the frontend (needs `trunk` + the wasm32 target, see frontend/README.md)
cd frontend && trunk build --release && cd ..

# 3. Serve API + UI from one binary
cargo run -p veridex-server -- --db crawled.db --port 8080 \
  --static-dir frontend/dist

# 4. Open http://127.0.0.1:8080 — search, briefs, reader, index stats
```

Without `--static-dir` you get the API only; during frontend development
`cd frontend && trunk serve` proxies `/api` to `127.0.0.1:8080` (see
[`frontend/README.md`](frontend/README.md)).

The API is also usable directly:

```sh
curl -s 'http://localhost:8080/api/search?q=consciousness+knowledge' | jq

curl -s -X POST http://localhost:8080/api/summarize \
  -H 'content-type: application/json' \
  -d '{"query":"consciousness knowledge","format":"pdf"}' \
  -o brief.pdf
```

See [`server/README.md`](server/README.md) for the full API reference.

## How ranking works

Results are ordered by a hybrid score that deliberately weights source
quality above raw term matching:

```
score = 0.55 · relevance + 0.40 · authority + 0.05 · title_coverage
```

* **Authority (40%)** — curated trusted/demoted host lists, TLD rules
  (`.edu`, `.gov`, `.ac.*`, …) and page signals: DOI, references section,
  inline citations, long-form content, PDF availability. Pages get a letter
  grade `A`–`D`.
* **Relevance (55%)** — SQLite FTS5 BM25 (porter stemming) squashed with
  `s / (s + 2)`. The absolute squash keeps the `IDF ≈ 0` epsilon produced by
  terms that match most of the corpus at ~0 instead of amplifying noise.
* **Title coverage (5%)** — query terms present in the page title.

Because authority carries 40% of the score, a well-sourced academic page can
outrank a keyword-stuffed popular page — which is the point.

## Summarization

Entirely local and deterministic:

1. Run the query, take the top authority-ranked pages.
2. Split pages into sentences. Crawler text is hard-wrapped, so single
   newlines are soft wraps; blank lines and bullet lines are boundaries.
   Numbered section headings (`4.2`, `4.4.1`), footnote anchors (`[46]`) and
   the leading page title are dropped; decimals (`3.5`) and abbreviations
   (`Sect.`, `et al.`) never split.
3. Score sentences on query coverage, salience, position, length and source
   authority.
4. Select with MMR (maximal marginal relevance) so the brief is diverse and
   non-redundant, capped per page so no single source dominates.
5. Render to JSON or a typeset PDF (masthead, key findings with citation
   markers, source breakdown with grades/authority, references, and
   `page N of M` footers).

## Crawler

```sh
cargo run -p web_weaver -- crawl -f crawler/seeds/scholarly.txt -d 1
```

Defaults: 20 concurrent parses, 50 requests/s global politeness cap,
`max-depth 3`. `robots.txt` and per-host crawl delays are respected. The
frontier persists in the database, so an interrupted crawl resumes. Only
successful 2xx HTML responses are indexed (404/error pages are skipped).

`crawler/seeds/scholarly.txt` contains verified Stanford Encyclopedia of
Philosophy entry URLs. Edit freely; one URL per line, `#` comments allowed.

## Live search

Search normally covers only pages that have been crawled into the index
(`crawled.db`).

A **"Live web"** toggle (default **on**) is available on the search page and
the research-brief page. When on, each query fans out to a discovery backend,
fetches the top not-yet-indexed pages, stores them, and re-ranks — so live
results are fully usable by research briefs.

```sh
# Toggle lives in the UI (default on); API usage:
curl -s 'http://127.0.0.1:8080/api/search?q=perovskite+solar+cell&live=true'
```

Implementation lives in `server/src/live.rs`; discovery uses DuckDuckGo's
HTML endpoint with a Bing fallback, and fetched pages go through the same
extraction pipeline as the crawler.

## Frontend

`frontend/` is a Leptos 0.8 single-page app compiled to WebAssembly
(`trunk build`). Hash routing (`#/`, `#/brief?q=…`, `#/page/{id}`,
`#/stats`) keeps it deployable on any static host. Four views:

* **Search** — query + optional domain filter, result cards with grade
  badges, hybrid/relevance/authority scores, source-signal chips, and
  pagination.
* **Research brief** — extractive summary with findings and per-source
  passages, plus one-click PDF export (fetched as a blob and handed to the
  browser's download manager).
* **Page reader** — full stored content, reflowed from the crawler's
  hard-wrapped text, with authority breakdown.
* **Index stats** — page count, crawl range, top hosts.

See [`frontend/README.md`](frontend/README.md) for toolchain setup, the dev
server, and how to point the app at a remote API.

## Tests

```sh
cargo test                              # 75 tests (core + server)
cargo clippy --all-targets -- -D warnings

cd frontend
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
trunk build --release                   # full app build
```
