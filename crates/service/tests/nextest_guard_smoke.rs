mod common;

#[test]
fn nextest_without_database_url_panics_clearly() {
    // Run the guard inline — in practice it's called from the
    // shared_migrated_pool() / acquire() entry points.
    let saved_nextest = std::env::var("NEXTEST").ok();
    let saved_db_url = std::env::var("DATABASE_URL").ok();

    // Safety note: we're a standalone test binary with exactly one test
    // function; no concurrent thread touches NEXTEST/DATABASE_URL during
    // this window. Promoting this test out of `common/mod.rs` (where it
    // was compiled into every integration-test binary and raced with
    // parallel threads) is the whole point of the split.
    std::env::set_var("NEXTEST", "1");
    std::env::remove_var("DATABASE_URL");

    let result = std::panic::catch_unwind(common::guard_nextest_requires_shared_db);

    match saved_nextest {
        Some(v) => std::env::set_var("NEXTEST", v),
        None => std::env::remove_var("NEXTEST"),
    };
    match saved_db_url {
        Some(v) => std::env::set_var("DATABASE_URL", v),
        None => std::env::remove_var("DATABASE_URL"),
    };

    let err = result.expect_err("must panic");
    let msg = err
        .downcast_ref::<String>()
        .map(|s| s.as_str())
        .or_else(|| err.downcast_ref::<&'static str>().copied())
        .unwrap_or("<non-string panic>");
    assert!(
        msg.contains("DATABASE_URL"),
        "message must mention DATABASE_URL; got: {msg}"
    );
    assert!(
        msg.contains("cargo xtask test"),
        "message must point at xtask; got: {msg}"
    );
}
