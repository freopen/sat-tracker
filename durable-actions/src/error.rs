use crate::action::{ActionId, HandlerError};

/// Errors from setup and queue operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SQLite operation failed: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("SQLite migration failed: {0}")]
    Migration(#[from] rusqlite_migration::Error),
    #[error("background SQLite task failed: {0}")]
    DatabaseTask(#[from] tokio::task::JoinError),
    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("action name must not be empty")]
    EmptyActionName,
    #[error("action `{0}` was registered more than once")]
    DuplicateAction(String),
    #[error("persisted action `{0}` has no registered handler")]
    UnknownAction(String),
    #[error("a runner is already active for database `{0}`")]
    AlreadyRunning(String),
    #[error("failed to resolve the database path: {0}")]
    Path(#[source] std::io::Error),
    #[error("the runner is shutting down")]
    ShuttingDown,
    #[error("scheduled time is outside the range supported by SystemTime")]
    ScheduleOverflow,
    #[error("transactional action operations are only available inside an action")]
    NoActionContext,
    #[error("the transactional action context is already borrowed")]
    ActionContextBusy,
}

/// Errors that terminate the runner.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error(transparent)]
    Engine(#[from] Error),
    #[error("action {id} (`{name}`) failed: {source}")]
    Handler {
        id: ActionId,
        name: String,
        #[source]
        source: HandlerError,
    },
    #[error("runner task failed: {0}")]
    Task(tokio::task::JoinError),
}
