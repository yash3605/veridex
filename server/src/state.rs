//! Shared application state passed to every handler.

use veridex_core::SqlitePool;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
}
