mod cli;
use cli::Command;
mod crawler;
mod politeness;

use clap::Parser;
use politeness::PolitenessPolicy;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use url::Url;
use veridex_core::db;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let url_to_fetch: Arc<Mutex<VecDeque<(Url, u32)>>> = Arc::new(Mutex::new(VecDeque::new()));
    let visited_url: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));

    let cli = cli::Cli::parse();

    let pool = db::connect(&cli.db).await?;
    db::init_schema(&pool).await?;

    match cli.command {
        Command::Crawl {
            url,
            concurrent_parse,
            rate_limiter,
            seed_file,
            max_depth,
        } => {
            let saved_urls = db::load_queue(&pool).await?;
            for (url_str, depth) in &saved_urls {
                if let Ok(url) = Url::parse(url_str) {
                    url_to_fetch
                        .lock()
                        .expect("url frontier lock poisoned")
                        .push_back((url, *depth));
                }
            }

            let seed_urls: Vec<Url> = if let Some(urls_list) = url {
                urls_list
                    .iter()
                    .map(|u| Url::parse(u.as_str()))
                    .collect::<Result<Vec<_>, _>>()?
            } else if let Some(file) = seed_file {
                let file_contents = fs::read_to_string(file)?;
                file_contents
                    .lines()
                    .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
                    .map(Url::parse)
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                anyhow::bail!("At least give one argument either URL or file to seed")
            };
            let polite_limiter = Arc::new(PolitenessPolicy::new(rate_limiter as u32));
            let semaphore = Arc::new(Semaphore::new(concurrent_parse as usize));

            if let Err(e) = crawler::fetch_and_parse(
                seed_urls,
                visited_url,
                url_to_fetch,
                polite_limiter,
                semaphore,
                pool,
                max_depth,
            )
            .await
            {
                tracing::error!("Error: {}", e);
            }
        }
        Command::Search { query, page } => {
            let results = db::search(&pool, &query, page).await?;
            for result in results {
                println!("URL: {}", result.url);
                println!("Title: {}", result.title.unwrap_or_default());
                println!("Description: {}", result.description.unwrap_or_default());
                println!("-----------");
            }

            tracing::info!("Search with query is done");
        }
    }

    Ok(())
}
