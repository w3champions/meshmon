//! Smoke test verifying the hostname fixtures are wired through
//! `AppState::new` and exposed on the state handle. Protects against
//! a future refactor that drops one of the three handles or inverts
//! their ordering — the specific assertions are chosen so either
//! regression surfaces as a field-not-found or a moved-semantics
//! compile error rather than a runtime mis-wire.

mod common;

#[tokio::test]
async fn state_with_admin_wires_hostname_fixtures() {
    let pool = common::shared_migrated_pool().await.clone();
    let state = common::state_with_admin(pool).await;

    // Broadcaster starts empty — no sessions are registered by
    // `state_with_admin`.
    assert_eq!(state.hostname_broadcaster.session_count(), 0);

    // Refresh limiter starts with no per-session buckets.
    assert_eq!(state.hostname_refresh_limiter.bucket_count(), 0);

    // Resolver handle exposes its broadcaster; verify it resolves
    // (the exact identity is an implementation detail, but the
    // accessor is part of the public API).
    let _broadcaster_ref = state.hostname_resolver.broadcaster();
}
