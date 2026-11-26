//! Minimal hash router: `#/`, `#/brief?q=…`, `#/page/{id}`, `#/stats`.
//!
//! Hash routing keeps the built app deployable on any static file server
//! (no SPA fallback rules needed) while still giving shareable URLs.

use std::collections::HashMap;

use leptos::prelude::*;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Search {
        q: Option<String>,
        page: usize,
        domain: Option<String>,
    },
    Brief {
        q: Option<String>,
    },
    Page {
        id: i64,
    },
    Stats,
    NotFound,
}

/// Reactive current route, updated on every `hashchange`.
pub fn use_hash_route() -> RwSignal<Route> {
    let current = RwSignal::new(parse_hash());

    if let Some(window) = web_sys::window() {
        let cb = Closure::<dyn FnMut()>::new(move || current.set(parse_hash()));
        let _ = window
            .add_event_listener_with_callback("hashchange", cb.as_ref().unchecked_ref());
        // Lives for the lifetime of the app.
        cb.forget();
    }

    current
}

pub fn parse_hash() -> Route {
    let hash = web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .unwrap_or_default();
    parse(&hash)
}

pub fn parse(hash: &str) -> Route {
    let without_hash = hash.strip_prefix('#').unwrap_or(hash);
    let (path, query) = match without_hash.split_once('?') {
        Some((p, q)) => (p, q),
        None => (without_hash, ""),
    };
    let path = path.trim_start_matches('/');
    let params = parse_query(query);

    match path {
        "" => Route::Search {
            q: params.get("q").cloned(),
            page: params
                .get("page")
                .and_then(|p| p.parse().ok())
                .unwrap_or(0),
            domain: params.get("domain").cloned(),
        },
        "brief" => Route::Brief {
            q: params.get("q").cloned(),
        },
        "stats" => Route::Stats,
        _ if path.starts_with("page/") => match path["page/".len()..].parse::<i64>() {
            Ok(id) => Route::Page { id },
            Err(_) => Route::NotFound,
        },
        _ => Route::NotFound,
    }
}

fn parse_query(qs: &str) -> HashMap<String, String> {
    qs.split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((decode(key), decode(value)))
        })
        .collect()
}

fn decode(s: &str) -> String {
    let s = s.replace('+', " ");
    js_sys::decode_uri_component(&s)
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or(s)
}

pub fn enc(s: &str) -> String {
    js_sys::encode_uri_component(s)
        .as_string()
        .unwrap_or_else(|| s.to_string())
}

/// Navigates by setting the location hash (without the leading `#`).
pub fn go(hash: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_hash(hash);
    }
}

pub fn search_url(q: &str, page: usize, domain: Option<&str>) -> String {
    let mut url = format!("/?q={}", enc(q.trim()));
    if page > 0 {
        url.push_str(&format!("&page={page}"));
    }
    if let Some(domain) = domain.filter(|d| !d.trim().is_empty()) {
        url.push_str(&format!("&domain={}", enc(domain.trim())));
    }
    url
}

pub fn brief_url(q: &str) -> String {
    format!("/brief?q={}", enc(q.trim()))
}
