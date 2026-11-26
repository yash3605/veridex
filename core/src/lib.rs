//! Shared data layer for Veridex.
//!
//! Provides the SQLite schema, page storage, FTS5 search primitives and
//! query-building utilities used by both the crawler and the search server.

pub mod db;
pub mod extract;
pub mod query;

pub use sqlx::SqlitePool;
