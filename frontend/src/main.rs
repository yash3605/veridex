//! Veridex frontend — a small CSR (client-side rendered) Leptos app.
//!
//! Routing is hash-based (`#/search`, `#/brief?q=…`, `#/page/42`, `#/stats`)
//! so the built `dist/` can be served from any static host without server
//! rewrite rules.

mod api;
mod components;
mod pages;
mod route;

use leptos::prelude::*;

use route::Route;

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    let route = route::use_hash_route();

    view! {
        <div class="app">
            <header class="topbar">
                <div class="bar-inner">
                    <a class="brand" href="#/">
                        <span class="brand-mark">"V"</span>
                        <span class="brand-name">"Veridex"</span>
                    </a>
                    <span class="tagline">"Scholarly-first search"</span>
                    <nav class="nav">
                        <a href="#/">"Search"</a>
                        <a href="#/stats">"Index"</a>
                    </nav>
                </div>
            </header>

            <main class="main">
                {move || match route.get() {
                    Route::Search { q, page, domain } => view! {
                        <pages::search::SearchPage q=q page=page domain=domain/>
                    }
                    .into_any(),

                    Route::Brief { q } => view! { <pages::brief::BriefPage q=q/> }.into_any(),

                    Route::Page { id } => view! { <pages::reader::PageReader id=id/> }.into_any(),

                    Route::Stats => view! { <pages::stats::StatsPage/> }.into_any(),

                    Route::NotFound => view! {
                        <div class="empty">
                            <h2>"Page not found"</h2>
                            <p><a href="#/">"Back to search"</a></p>
                        </div>
                    }
                    .into_any(),
                }}
            </main>

            <footer class="footer">
                <span>"Veridex · local scholarly index"</span>
                <span class="footer-note">"Rust · WebAssembly · no trackers"</span>
            </footer>
        </div>
    }
}
