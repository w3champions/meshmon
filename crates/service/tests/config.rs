//! Integration tests for `meshmon_service::config`. Pool-free; no Docker
//! required.

use meshmon_service::config::{Config, LogFormat};

const MINIMAL: &str = r#"
[service]
listen_addr = "127.0.0.1:8080"

[database]
url = "postgres://meshmon:secret@localhost:5432/meshmon"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#;

#[test]
fn minimal_config_parses() {
    let cfg = Config::from_str(MINIMAL, "test.toml").expect("parse");

    assert_eq!(cfg.service.listen_addr.to_string(), "127.0.0.1:8080");
    assert_eq!(
        cfg.database.url(),
        "postgres://meshmon:secret@localhost:5432/meshmon"
    );
    assert!(cfg.auth.users.is_empty());
    assert_eq!(cfg.logging.format, LogFormat::Json);
    assert_eq!(cfg.logging.filter, "info");
}

use meshmon_service::error::BootError;

#[test]
fn parse_error_has_path_context() {
    let err = Config::from_str("this is ::: not toml", "oops.toml").unwrap_err();
    assert!(matches!(err, BootError::ConfigParse { path, .. } if path == "oops.toml"));
}

#[test]
fn missing_database_section_rejected() {
    let err = Config::from_str(
        r#"
[service]
listen_addr = "127.0.0.1:8080"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(err, BootError::ConfigParse { .. }));
}

#[test]
fn bad_listen_addr_rejected() {
    let err = Config::from_str(
        r#"
[service]
listen_addr = "not-an-address"
[database]
url = "postgres://a@b/c"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        BootError::ConfigInvalid { reason, .. } if reason.contains("listen_addr")
    ));
}

#[test]
fn url_env_resolves() {
    // Unique var name to avoid collisions with other tests in the process.
    std::env::set_var("MESHMON_T04_TEST_URL", "postgres://u:p@host:5432/db");
    let cfg = Config::from_str(
        r#"
[database]
url_env = "MESHMON_T04_TEST_URL"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "t.toml",
    )
    .expect("parse");
    assert_eq!(cfg.database.url(), "postgres://u:p@host:5432/db");
    std::env::remove_var("MESHMON_T04_TEST_URL");
}

#[test]
fn url_env_missing_is_error() {
    std::env::remove_var("MESHMON_T04_TEST_URL_UNSET");
    let err = Config::from_str(
        r#"
[database]
url_env = "MESHMON_T04_TEST_URL_UNSET"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        BootError::EnvMissing { name, .. } if name == "MESHMON_T04_TEST_URL_UNSET"
    ));
}

#[test]
fn neither_url_nor_url_env_is_error() {
    let err = Config::from_str(
        r#"
[database]
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        BootError::ConfigInvalid { reason, .. } if reason.contains("database.url")
    ));
}

#[test]
fn invalid_phc_hash_rejected() {
    let err = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[[auth.users]]
username = "admin"
password_hash = "plaintext-not-phc"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        BootError::ConfigInvalid { reason, .. } if reason.contains("PHC")
    ));
}

#[test]
fn valid_argon2_phc_accepted() {
    // Sample hash from argon2 docs. Purely structural validation — no
    // password verification happens at load time.
    let err_or_ok = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[[auth.users]]
username = "admin"
password_hash = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHQ$BztdyfEefG5V18Uudy4vk6vVrWxD1w9dDLV5GhJNDAs"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "t.toml",
    );
    assert!(err_or_ok.is_ok(), "{err_or_ok:?}");
    let cfg = err_or_ok.unwrap();
    assert_eq!(cfg.auth.users.len(), 1);
    assert_eq!(cfg.auth.users[0].username, "admin");
}

#[test]
fn log_format_compact_accepted() {
    let cfg = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"
[logging]
format = "compact"
[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "t.toml",
    )
    .expect("parse");
    assert_eq!(cfg.logging.format, LogFormat::Compact);
}

#[test]
fn log_format_unknown_rejected() {
    let err = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"
[logging]
format = "yaml"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        BootError::ConfigInvalid { reason, .. } if reason.contains("logging.format")
    ));
}

#[test]
fn shipped_example_parses() {
    std::env::set_var(
        "MESHMON_POSTGRES_URL",
        "postgres://admin:pw@localhost:5432/db",
    );
    std::env::set_var("MESHMON_AGENT_TOKEN", "dummy-token");
    // Added by T24: meshmon.example.toml now references this env var
    // from its [[auth.users]] entry.
    std::env::set_var(
        "MESHMON_ADMIN_PASSWORD_HASH",
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY",
    );

    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/meshmon.example.toml"
    );
    let text = std::fs::read_to_string(path).expect("read example toml");
    let cfg = Config::from_str(&text, path).expect("parse shipped example");

    assert_eq!(cfg.service.listen_addr.to_string(), "0.0.0.0:8080");
    assert_eq!(cfg.database.url(), "postgres://admin:pw@localhost:5432/db");
    assert_eq!(cfg.agent_api.shared_token.as_deref(), Some("dummy-token"));
    // Added by T24: verify the env-indirected admin hash resolved.
    assert_eq!(cfg.auth.users.len(), 1);
    assert_eq!(cfg.auth.users[0].username, "admin");

    std::env::remove_var("MESHMON_POSTGRES_URL");
    std::env::remove_var("MESHMON_AGENT_TOKEN");
    std::env::remove_var("MESHMON_ADMIN_PASSWORD_HASH");
}

#[test]
fn empty_env_var_value_rejected() {
    // Operator typo: VAR="" silently resolving to a blank secret is exactly
    // what produces opaque downstream failures ("connection string is
    // invalid"). Reject at load time instead.
    std::env::set_var("MESHMON_T04_TEST_EMPTY_URL", "");
    let err = Config::from_str(
        r#"
[database]
url_env = "MESHMON_T04_TEST_EMPTY_URL"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(
        matches!(
            &err,
            BootError::ConfigInvalid { reason, .. }
                if reason.contains("MESHMON_T04_TEST_EMPTY_URL") && reason.contains("empty")
        ),
        "unexpected: {err:?}"
    );
    std::env::remove_var("MESHMON_T04_TEST_EMPTY_URL");
}

#[test]
fn zero_shutdown_deadline_rejected() {
    let err = Config::from_str(
        r#"
[service]
shutdown_deadline_seconds = 0

[database]
url = "postgres://a@b/c"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        err,
        BootError::ConfigInvalid { reason, .. }
            if reason.contains("shutdown_deadline_seconds")
    ));
}

#[test]
fn empty_optional_env_var_value_rejected() {
    // Same rationale as required-secret empty rejection: an opt-in env
    // reference with a blank value is almost certainly a deploy-pipeline
    // typo, not an intentional "token disabled" signal. Operators disable
    // the agent API by leaving both `shared_token` and `shared_token_env`
    // unset. The error must distinguish "set but empty" (ConfigInvalid)
    // from "not set at all" (EnvMissing) so the operator-facing message
    // actually matches reality.
    std::env::set_var("MESHMON_T04_TEST_EMPTY_TOKEN", "");
    let err = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[agent_api]
shared_token_env = "MESHMON_T04_TEST_EMPTY_TOKEN"
"#,
        "t.toml",
    )
    .unwrap_err();
    assert!(matches!(
        &err,
        BootError::ConfigInvalid { reason, .. }
            if reason.contains("MESHMON_T04_TEST_EMPTY_TOKEN")
                && reason.contains("set but empty")
    ));
    std::env::remove_var("MESHMON_T04_TEST_EMPTY_TOKEN");
}

#[test]
fn trust_forwarded_headers_defaults_to_false() {
    let cfg = Config::from_str(
        r#"
[database]
url = "postgres://u@h/d"
[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "test.toml",
    )
    .expect("parse");
    assert!(!cfg.service.trust_forwarded_headers);
}

#[test]
fn trust_forwarded_headers_honored() {
    let cfg = Config::from_str(
        r#"
[database]
url = "postgres://u@h/d"
[service]
trust_forwarded_headers = true
[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "test.toml",
    )
    .expect("parse");
    assert!(cfg.service.trust_forwarded_headers);
}

#[test]
fn agents_section_defaults_when_missing() {
    let cfg = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "test.toml",
    )
    .expect("parse");
    assert_eq!(cfg.agents.target_active_window_minutes, 5);
    assert_eq!(cfg.agents.refresh_interval_seconds, 10);
}

#[test]
fn agents_section_parses_overrides() {
    let cfg = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[agents]
target_active_window_minutes = 15
refresh_interval_seconds = 30

[probing]
udp_probe_secret = "hex:0011223344556677"
"#,
        "test.toml",
    )
    .expect("parse");
    assert_eq!(cfg.agents.target_active_window_minutes, 15);
    assert_eq!(cfg.agents.refresh_interval_seconds, 30);
}

#[test]
fn agents_section_rejects_zero_values() {
    let err = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[agents]
refresh_interval_seconds = 0
"#,
        "test.toml",
    )
    .expect_err("must reject zero cadence");
    assert!(format!("{err}").contains("refresh_interval_seconds"));
}

#[test]
fn agents_section_rejects_zero_window() {
    let err = Config::from_str(
        r#"
[database]
url = "postgres://a@b/c"

[agents]
target_active_window_minutes = 0
"#,
        "test.toml",
    )
    .expect_err("must reject zero window");
    assert!(format!("{err}").contains("target_active_window_minutes"));
}

#[test]
fn metrics_auth_section_parses_inline_hash() {
    let toml = r#"
[database]
url = "postgres://a@b/c"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service.metrics_auth]
username = "prom"
password_hash = "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY"
"#;
    let cfg = Config::from_str(toml, "t.toml").expect("parse");
    let auth = cfg
        .service
        .metrics_auth
        .as_ref()
        .expect("metrics_auth present");
    assert_eq!(auth.username, "prom");
    assert!(auth.password_hash.starts_with("$argon2id$"));
}

#[test]
fn metrics_auth_section_rejects_malformed_hash() {
    let toml = r#"
[database]
url = "postgres://a@b/c"

[service.metrics_auth]
username = "prom"
password_hash = "not-a-phc-hash"
"#;
    let err = Config::from_str(toml, "t.toml").unwrap_err();
    assert!(err.to_string().contains("metrics_auth"));
}

#[test]
fn metrics_auth_section_resolves_env_hash() {
    std::env::set_var(
        "MESHMON_TEST_METRICS_PASSWORD_HASH",
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY",
    );
    let toml = r#"
[database]
url = "postgres://a@b/c"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service.metrics_auth]
username = "prom"
password_hash_env = "MESHMON_TEST_METRICS_PASSWORD_HASH"
"#;
    let cfg = Config::from_str(toml, "t.toml").expect("parse");
    let auth = cfg.service.metrics_auth.unwrap();
    assert!(auth.password_hash.starts_with("$argon2id$"));
    std::env::remove_var("MESHMON_TEST_METRICS_PASSWORD_HASH");
}

#[test]
fn metrics_auth_absent_is_none() {
    let toml = r#"
[database]
url = "postgres://a@b/c"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#;
    let cfg = Config::from_str(toml, "t.toml").expect("parse");
    assert!(cfg.service.metrics_auth.is_none());
}

#[test]
fn metrics_auth_section_rejects_both_hash_forms() {
    // Operator error: inline AND env-indirected. Fail loudly instead of
    // silently picking one — either of them may have been the intended
    // credential and the other a leftover.
    let toml = r#"
[database]
url = "postgres://a@b/c"

[service.metrics_auth]
username = "prom"
password_hash = "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY"
password_hash_env = "MESHMON_TEST_METRICS_PASSWORD_HASH_BOTH"
"#;
    let err = Config::from_str(toml, "t.toml").unwrap_err();
    assert!(
        matches!(
            &err,
            BootError::ConfigInvalid { reason, .. } if reason.contains("metrics_auth")
        ),
        "unexpected: {err:?}"
    );
}

#[test]
fn metrics_auth_section_rejects_no_hash_form() {
    // Username is set but neither password_hash nor password_hash_env is —
    // an incomplete opt-in that must not silently disable auth.
    let toml = r#"
[database]
url = "postgres://a@b/c"

[service.metrics_auth]
username = "prom"
"#;
    let err = Config::from_str(toml, "t.toml").unwrap_err();
    assert!(
        matches!(
            &err,
            BootError::ConfigInvalid { reason, .. } if reason.contains("metrics_auth")
        ),
        "unexpected: {err:?}"
    );
}

#[test]
fn metrics_auth_section_rejects_empty_username() {
    // Whitespace-only usernames are a deploy-pipeline typo, not a
    // legitimate "no principal" signal.
    let toml = r#"
[database]
url = "postgres://a@b/c"

[service.metrics_auth]
username = "   "
password_hash = "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY"
"#;
    let err = Config::from_str(toml, "t.toml").unwrap_err();
    assert!(
        matches!(
            &err,
            BootError::ConfigInvalid { reason, .. }
                if reason.contains("metrics_auth") && reason.contains("username")
        ),
        "unexpected: {err:?}"
    );
}

#[test]
fn metrics_auth_section_rejects_missing_env() {
    // Opt-in env-var reference pointing at an unset variable must surface
    // as EnvMissing so the operator-facing message names the variable
    // AND the config key (matches resolve_secret / resolve_optional_secret
    // taxonomy in crates/service/src/config.rs).
    std::env::remove_var("DOES_NOT_EXIST_MESHMON_T10_TEST");
    let toml = r#"
[database]
url = "postgres://a@b/c"

[service.metrics_auth]
username = "prom"
password_hash_env = "DOES_NOT_EXIST_MESHMON_T10_TEST"
"#;
    let err = Config::from_str(toml, "t.toml").unwrap_err();
    let rendered = err.to_string();
    assert!(
        matches!(
            &err,
            BootError::EnvMissing { name, key }
                if name == "DOES_NOT_EXIST_MESHMON_T10_TEST"
                    && key == "service.metrics_auth.password_hash"
        ),
        "unexpected: {err:?}"
    );
    assert!(
        rendered.contains("DOES_NOT_EXIST_MESHMON_T10_TEST")
            && rendered.contains("service.metrics_auth.password_hash"),
        "unexpected rendering: {rendered}"
    );
}
