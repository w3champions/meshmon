use std::process::Command;

fn xtask(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--package", "xtask", "--"])
        .args(args)
        .output()
        .expect("spawn xtask")
}

// These tests shell to `docker`. Skip with MESHMON_SKIP_DOCKER_TESTS=1.
fn docker_available() -> bool {
    std::env::var("MESHMON_SKIP_DOCKER_TESTS").is_err()
        && Command::new("docker")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

#[test]
fn status_reports_not_running_when_container_absent() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }
    let _ = xtask(&["test-db", "down"]); // cleanup any leftover
    let out = xtask(&["test-db", "status"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "xtask test-db status must exit 0");
    assert!(stdout.contains("not running"), "stdout: {stdout}");
}

#[test]
fn up_is_idempotent_and_prints_database_url() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }
    let _ = xtask(&["test-db", "down"]);

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

    let _ = xtask(&["test-db", "down"]);
}

#[test]
fn xtask_test_runs_nextest_against_shared_db() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }
    if Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("skip: cargo-nextest not installed locally");
        return;
    }
    let _ = xtask(&["test-db", "down"]);

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

    let _ = xtask(&["test-db", "down"]);
}
