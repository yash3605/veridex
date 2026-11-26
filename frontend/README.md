# Veridex Frontend

Leptos 0.8 CSR (client-side rendered) SPA compiled to WebAssembly with
[Trunk](https://trunkrs.dev). It talks to the axum backend over `/api/...`
relative URLs.

This crate is intentionally **excluded from the backend workspace** so the
root `cargo test` / `clippy` stay fast and host-only. Build it with Trunk
(or plain `cargo` for type-checking).

## Toolchain (one time)

```sh
rustup target add wasm32-unknown-unknown
# Trunk: prebuilt binary or `cargo install trunk --locked`
curl -sL -o /tmp/trunk.tar.gz \
  https://github.com/trunk-rs/trunk/releases/latest/download/trunk-x86_64-unknown-linux-gnu.tar.gz
tar -xzf /tmp/trunk.tar.gz -C ~/.cargo/bin && chmod +x ~/.cargo/bin/trunk
```

## Development

```sh
# terminal 1 — backend on 8080
cargo run -p veridex-server -- --db crawled.db --port 8080

# terminal 2 — dev server on 3000, hot-reloading
cd frontend && trunk serve
```

Open <http://127.0.0.1:3000>. The Trunk config (`Trunk.toml`) proxies
`/api/*` to `127.0.0.1:8080/api/*`, so the app runs same-origin and there is
no CORS or mixed-port juggling.

## Production build

```sh
cd frontend && trunk build --release      # output in frontend/dist/
```

Serve it in either of two ways:

1. **Single binary (recommended):** let the API server host it —
   ```sh
   cargo run -p veridex-server -- --db crawled.db --port 8080 \
     --static-dir frontend/dist
   ```
   Then everything is at <http://127.0.0.1:8080>. Hash routing means no SPA
   rewrite rules are needed.

2. **Any static host:** copy `dist/` anywhere. If the app is *not* on the
   same origin as the API, set the API base URL once in the browser console:
   ```js
   localStorage.setItem("veridex_api", "http://127.0.0.1:8080")
   ```
   (The backend sends permissive CORS headers, so cross-origin works.)

## Layout

```
frontend/
├── Trunk.toml          dev server + /api proxy
├── index.html          trunk entry
├── style.css           design system (cards, badges, chips, reader)
└── src/
    ├── main.rs         app shell + route dispatch
    ├── route.rs        hash router (#/, #/brief, #/page/:id, #/stats)
    ├── api.rs          typed client for every backend endpoint (+ PDF blob download)
    ├── components.rs   shared bits (grade badges, signal labels, live toggle)
    └── pages/
        ├── search.rs   query form, result cards, pagination
        ├── brief.rs    findings, sources, PDF export
        ├── reader.rs   full page content (reflows crawler hard-wraps)
        └── stats.rs    index overview, top hosts
```

## Live web toggle

The search page and the research-brief page each carry a **"Live web"** switch
(default **on**). When on, the app sends `live=true` to the search and
summarize endpoints; when the backend fetches and stores new pages during the
query, a `+N new pages from the live web` chip appears in the results.
Turning the toggle off reproduces the instant, index-only search.

## Checks

```sh
cargo clippy --target wasm32-unknown-unknown --all-targets -- -D warnings
trunk build --release
```

The UI is verified end-to-end with a real browser (Firefox headless via
geckodriver): search results, brief generation, reader, and stats all render
and call the live API.