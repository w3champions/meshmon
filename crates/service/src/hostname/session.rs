//! Session-id derivation shared by every handler that needs to scope
//! hostname work to a specific caller.
//!
//! `auth_session.session.id()` returns `Option<tower_sessions::Id>`; an
//! authenticated session always carries one, so the fallback only keeps
//! the signature total — `login_required!` already rejects anonymous
//! callers before any handler runs.

use crate::{hostname::SessionId, http::auth::AuthSession};

/// Resolve an [`AuthSession`] extractor to a stable [`SessionId`].
pub(crate) fn session_id_from_auth(auth_session: &AuthSession) -> SessionId {
    SessionId::new(
        auth_session
            .session
            .id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "no-session-id".to_string()),
    )
}
