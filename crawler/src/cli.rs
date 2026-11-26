use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(version, about)]
pub struct Cli {
    /// Path to the SQLite database file
    #[arg(long, global = true, default_value = "crawled.db")]
    pub db: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Crawl {
        #[arg(short, long)]
        url: Option<Vec<String>>,
        #[arg(short = 'f', long, num_args = 1)]
        seed_file: Option<String>,
        #[arg(short, long, default_value_t = 20)]
        concurrent_parse: u16,
        #[arg(short, long, default_value_t = 50)]
        rate_limiter: u16,
        #[arg(short = 'd', long, default_value_t = 3)]
        max_depth: u32,
    },

    Search {
        #[arg(short, long)]
        query: String,
        #[arg(short, long, default_value_t = 0)]
        page: u32,
    },
}
