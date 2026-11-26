use governor::{RateLimiter, clock::DefaultClock, state::keyed::DashMapStateStore};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use url::Url;

type PoliteLimiter = RateLimiter<String, DashMapStateStore<String>, DefaultClock>;

pub struct PolitenessPolicy {
    robots_cache: Arc<Mutex<HashMap<String, String>>>,
    pub polite_limiter: PoliteLimiter,
}

impl PolitenessPolicy {
    pub fn new(request_per_second: u32) -> Self {
        let quota = governor::Quota::per_second(
            NonZeroU32::new(request_per_second).expect("Must be non-zero"),
        );

        Self {
            robots_cache: Arc::new(Mutex::new(HashMap::new())),
            polite_limiter: RateLimiter::keyed(quota),
        }
    }

    pub async fn ensure_robots(&self, url: &Url) {
        let host = url.host_str().unwrap_or("Unknown").to_string();
        let has_key = self
            .robots_cache
            .lock()
            .expect("robots cache lock poisoned")
            .contains_key(&host);

        if !has_key {
            let scheme = url.scheme();
            tracing::info!("Fetching robots.txt for: {}", host);
            let robots = match reqwest::get(format!("{}://{}/robots.txt", scheme, host)).await {
                Ok(r) => r.text().await.unwrap_or_default(),
                Err(_) => String::new(),
            };
            self.robots_cache
                .lock()
                .expect("robots cache lock poisoned")
                .insert(host, robots);
        }
    }

    pub async fn is_allowed(&self, url: &Url) -> bool {
        let host = url.host_str().unwrap_or("Unknown").to_string();

        self.ensure_robots(url).await;

        let cache = self
            .robots_cache
            .lock()
            .expect("robots cache lock poisoned");
        let curr_robot = cache.get(&host).cloned().unwrap_or_default();

        robotstxt::DefaultMatcher::default().one_agent_allowed_by_robots(
            &curr_robot,
            "web_weaver",
            url.as_str(),
        )
    }
}
