//! Error types crossing the plugin/host boundary.
//!
//! Three layers, each with its own error type:
//!
//! - [`PluginError`] — failures internal to a plugin's lifecycle
//!   (init, start, stop, internal worker tasks).
//! - [`HostError`] — failures the host returns to plugins from
//!   `Host` method calls (permission denied, session unknown,
//!   storage error).
//! - [`TransportError`] — failures from a `TransportEngine`
//!   capability impl (connection lost, frame decode failed,
//!   delivery failed).
//!
//! All three implement `std::error::Error` so `?` and `anyhow`
//! work naturally.

use thiserror::Error;

/// Errors a plugin can produce during its lifecycle or runtime.
///
/// Plugins are encouraged to define their own error types using
/// `thiserror` and convert into `PluginError` at the boundary; this
/// type is the lingua franca for the supervisor.
#[derive(Debug, Error)]
pub enum PluginError {
    /// Plugin configuration was invalid — typically caught at
    /// `init`. Should include the offending key and reason.
    #[error("plugin configuration is invalid: {0}")]
    InvalidConfig(String),

    /// A required dependency (another plugin, an external service)
    /// is unavailable.
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),

    /// Initialisation failed for a reason that doesn't fit the
    /// other variants.
    #[error("plugin initialisation failed: {0}")]
    InitFailed(String),

    /// `start` failed — the plugin could not begin serving.
    #[error("plugin start failed: {0}")]
    StartFailed(String),

    /// `stop` failed or exceeded its deadline. The supervisor
    /// records this and continues shutting down other plugins.
    #[error("plugin stop failed: {0}")]
    StopFailed(String),

    /// Catch-all for transient runtime errors a plugin wants to
    /// surface. Prefer a specific variant where one applies.
    #[error("plugin error: {0}")]
    Runtime(String),

    /// Wrapper for any other concrete error type a plugin chooses
    /// to bubble up. The plugin's local error type implements
    /// `Into<PluginError>` via this variant.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Errors the host returns to plugins from `Host` method calls.
#[derive(Debug, Error)]
pub enum HostError {
    /// The caller's permission context doesn't satisfy the
    /// operation's required tier. Carries the required tier so
    /// the caller can render a meaningful message.
    #[error("permission denied: {required:?} required")]
    PermissionDenied {
        /// The minimum tier the operation requires.
        required: crate::permissions::PermissionLevel,
    },

    /// The given `SessionId` doesn't correspond to a live session.
    /// May indicate session expiry, an unknown session, or a
    /// transport bug (sending a stale ID).
    #[error("session not found: {0}")]
    UnknownSession(crate::identity::SessionId),

    /// The session exists but has no bound user, and the
    /// requested operation requires authentication.
    #[error("session is not authenticated")]
    NotAuthenticated,

    /// A user, room, or message the request referenced doesn't
    /// exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A precondition for the operation failed (e.g., sending a
    /// DM to a blocked user, posting to a room you can't read).
    /// The string is human-readable.
    #[error("precondition failed: {0}")]
    PreconditionFailed(String),

    /// A persistence-layer error. The string is suitable for
    /// logging; do not surface it to end users verbatim.
    #[error("storage error: {0}")]
    Storage(String),

    /// A configuration or system error that prevents the host
    /// from completing the request. Should be rare in steady
    /// state.
    #[error("internal host error: {0}")]
    Internal(String),

    /// The operation is not supported by this `Host` implementation.
    ///
    /// Default implementations of admin methods on the `Host` trait return
    /// this variant so `MockHost` and other minimal implementations compile
    /// without implementing every admin method.
    #[error("operation not supported: {0}")]
    NotSupported(String),
}

/// Errors a `TransportEngine` capability impl can produce.
#[derive(Debug, Error)]
pub enum TransportError {
    /// The transport's listener could not start (e.g., port in
    /// use, socket permissions, bridge unreachable).
    #[error("listener failed to start: {0}")]
    ListenerFailed(String),

    /// A delivery attempt failed and is not retryable.
    #[error("delivery failed: {0}")]
    DeliveryFailed(String),

    /// A delivery attempt failed but is worth retrying.
    /// Higher-level code may queue and retry.
    #[error("delivery would block: {0}")]
    WouldBlock(String),

    /// A connection to a backing service (the radio bridge, the
    /// admin web TLS terminator, etc.) has been lost.
    #[error("connection lost: {0}")]
    ConnectionLost(String),

    /// An incoming frame or request could not be parsed.
    #[error("malformed input: {0}")]
    Malformed(String),

    /// Wrapper for any other concrete error type the transport
    /// chooses to bubble up.
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SessionId;
    use crate::permissions::PermissionLevel;

    #[test]
    fn host_error_messages_are_useful() {
        let e = HostError::PermissionDenied {
            required: PermissionLevel::Sysop,
        };
        assert!(format!("{e}").contains("Sysop"));

        let e = HostError::UnknownSession(SessionId::__internal_new(42));
        let msg = format!("{e}");
        assert!(
            msg.contains("session:"),
            "expected session prefix in: {msg}"
        );
    }

    #[test]
    fn transport_error_other_variant_chain() {
        // A concrete error type can be wrapped via the Other
        // variant for free.
        #[derive(Debug, Error)]
        #[error("custom: {0}")]
        struct Custom(&'static str);

        let inner: Box<dyn std::error::Error + Send + Sync> = Box::new(Custom("nope"));
        let wrapped: TransportError = inner.into();
        assert!(format!("{wrapped}").contains("nope"));
    }

    #[test]
    fn plugin_error_other_variant_chain() {
        #[derive(Debug, Error)]
        #[error("plugin internal: {0}")]
        struct Inner(&'static str);

        let inner: Box<dyn std::error::Error + Send + Sync> = Box::new(Inner("oops"));
        let wrapped: PluginError = inner.into();
        assert!(format!("{wrapped}").contains("oops"));
    }
}
