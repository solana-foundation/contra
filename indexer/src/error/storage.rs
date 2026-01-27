/// Errors from database storage operations (PostgreSQL via sqlx)
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Query execution failed: {0}")]
    QueryFailed(#[from] sqlx::Error),

    #[error("Database error: {message}")]
    DatabaseError { message: String },
}
