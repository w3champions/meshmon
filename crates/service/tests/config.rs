//! Integration tests for `meshmon_service::config`. Pool-free; no Docker
//! required.

use meshmon_service::config::{Config, LogFormat};

const MINIMAL: &str = r#"
[service]
listen_addr = "127.0.0.1:8080"

[database]
url = "postgres://meshmon:secret@localhost:5432/meshmon"
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
