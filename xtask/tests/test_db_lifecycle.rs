use std::process::Command;

fn xtask(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--package", "xtask", "--"])
        .args(args)
        .output()
        .expect("spawn xtask")
}

// These scenarios shell to `docker`. Skip with MESHMON_SKIP_DOCKER_TESTS=1.
fn docker_available() -> bool {
    std::env::var("MESHMON_SKIP_DOCKER_TESTS").is_err()
        && Command::new("docker")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

/// Consolidated lifecycle test. All three scenarios share the single
/// `meshmon-test-pg` container name and therefore MUST run serially —
/// splitting them across `#[test]` functions lets `cargo test`'s default
/// parallel runner race on `docker run`, yielding a `Conflict` failure on
/// multi-core machines. Keep as one function.
#[test]
fn xtask_test_db_lifecycle() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }

    // Scenario 1: status reports "not running" when the container is absent.
    let _ = xtask(&["test-db", "down"]);
    let out = xtask(&["test-db", "status"]);
    assert!(out.status.success(), "xtask test-db status must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("not running"), "stdout: {stdout}");

    // Scenario 2: `up` is idempotent and prints the export line.
    let up1 = xtask(&["test-db", "up"]);
    assert!(up1.status.success(), "first up must succeed");
    let s1 = String::from_utf8_lossy(&up1.stdout);
    assert!(
        s1.contains("export DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres"),
        "stdout must print the export line; got: {s1}"
    );

    let up2 = xtask(&["test-db", "up"]);
    assert!(up2.status.success(), "second up must succeed (idempotent)");
    let s2 = String::from_utf8_lossy(&up2.stdout);
    assert!(
        s2.contains("already running"),
        "second up must note idempotency; got: {s2}"
    );

    // Scenario 3: `xtask test` runs nextest against the shared DB. Skips
    // gracefully when cargo-nextest isn't installed.
    let have_nextest = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if have_nextest {
        // Filter to a single known-fast test so the assertion is cheap.
        let out = xtask(&[
            "test",
            "--",
            "-E",
            "test(nextest_without_database_url_panics_clearly)",
        ]);
        assert!(
            out.status.success(),
            "xtask test must exit 0; stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    } else {
        eprintln!("skip scenario 3: cargo-nextest not installed");
    }

    // Cleanup — leave the host in the pre-test state.
    let _ = xtask(&["test-db", "down"]);
}
