use tracing_subscriber::{EnvFilter, fmt};
use uuid::Uuid;

/// Session-wide context for structured logging correlation.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: Uuid,
}

impl SessionContext {
    pub fn new() -> Self {
        Self {
            session_id: Uuid::new_v4(),
        }
    }
}

/// Initialize the tracing subscriber with structured JSON logging.
///
/// All log lines include `session_id` for correlation.
/// Use `--verbose` for DEBUG level, otherwise INFO.
pub fn init_logging(verbose: bool) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if verbose {
            EnvFilter::new("debug")
        } else {
            EnvFilter::new("info")
        }
    });

    fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .json()
        .init();
}

/// Initialize logging with human-readable format (for interactive CLI use).
pub fn init_logging_pretty(verbose: bool) {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        if verbose {
            EnvFilter::new("debug")
        } else {
            EnvFilter::new("info")
        }
    });

    fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_thread_ids(false)
        .init();
}
