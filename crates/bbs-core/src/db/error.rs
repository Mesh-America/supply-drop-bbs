//! Error types for the persistence layer.

/// Error returned by store operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness constraint was violated (e.g., duplicate username).
    #[error("conflict: {0}")]
    Conflict(String),

    /// A referential-integrity constraint was violated.
    #[error("referential integrity violated: {0}")]
    IntegrityViolation(String),

    /// The requested row does not exist.
    #[error("not found")]
    NotFound,

    /// A value fetched from the database could not be decoded into
    /// the expected Rust type (e.g., unknown enum discriminant).
    #[error("failed to decode database value: {0}")]
    Decode(String),

    /// A raw database error.
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}

/// Error returned when `Database::open` fails.
#[derive(Debug, thiserror::Error)]
pub enum DbOpenError {
    /// The underlying SQLite connection could not be established.
    #[error("database connection failed: {0}")]
    Connect(#[from] sqlx::Error),

    /// One or more pending migrations could not be applied.
    #[error("migration failed: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    /// The room linked-list invariant is violated in the database.
    /// The BBS refuses to start so the operator can repair the data.
    #[error("room walk-order invariant violated: {0}")]
    RoomOrder(String),
}
