//! Fork privacy policy: optional outbound uploads are permanently disabled.
//!
//! This fork never uploads traces, session archives, repo/code snapshots,
//! workspace environment dumps, share payloads, product telemetry, feedback
//! signals, session-registry metadata, or crash reports to remote services.
//! Auth and inference API calls are unaffected.
//!
//! Call sites should prefer the typed resolvers in [`crate::agent::config`] /
//! [`crate::auth::AuthManager`], which honor this policy. The constant exists
//! so other crates and CLI entry points can short-circuit without re-deriving
//! config precedence.

/// When `true`, every optional upload / telemetry / share path is forced off.
pub const OPTIONAL_UPLOADS_DISABLED: bool = true;

/// Runtime alias for [`OPTIONAL_UPLOADS_DISABLED`].
#[inline]
pub fn optional_uploads_disabled() -> bool {
    OPTIONAL_UPLOADS_DISABLED
}
