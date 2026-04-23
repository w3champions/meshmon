use std::process::Command;

fn xtask(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO"))
        .args(["run", "--quiet", "--package", "xtask", "--"])
        .args(args)
        .output()
        .expect("spawn xtask")
}

fn docker_available() -> bool {
    std::env::var("MESHMON_SKIP_DOCKER_TESTS").is_err()
        && Command::new("docker")
            .arg("version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn parse_export_line(stdout: &str) -> Option<(String, String)> {
    // Lines in stdout we care about:
    //   created meshmon-test-pg-1a2b3c4d on host port 54321
    //   export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:54321/postgres
    let mut name = None;
    let mut url = None;
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("created ") {
            if let Some((n, _)) = rest.split_once(' ') {
                name = Some(n.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("export DATABASE_URL=") {
            url = Some(rest.trim().to_string());
        }
    }
    Some((name?, url?))
}

/// Per-invocation containers MUST be parallel-safe. This test boots
/// two concurrently from one `cargo test` process; a regression to a
/// fixed name/port would surface as "container name already in use" or
/// "port already allocated".
#[test]
fn xtask_test_db_up_is_parallel_safe() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }

    let h1 = std::thread::spawn(|| xtask(&["test-db", "up"]));
    let h2 = std::thread::spawn(|| xtask(&["test-db", "up"]));
    let o1 = h1.join().expect("thread 1");
    let o2 = h2.join().expect("thread 2");

    assert!(
        o1.status.success(),
        "first up failed: {}",
        String::from_utf8_lossy(&o1.stderr)
    );
    assert!(
        o2.status.success(),
        "second up failed: {}",
        String::from_utf8_lossy(&o2.stderr)
    );

    let (name1, url1) =
        parse_export_line(&String::from_utf8_lossy(&o1.stdout)).expect("parse first up");
    let (name2, url2) =
        parse_export_line(&String::from_utf8_lossy(&o2.stdout)).expect("parse second up");

    assert_ne!(name1, name2, "names must differ across invocations");
    assert_ne!(url1, url2, "URLs must differ across invocations");
    assert!(name1.starts_with("meshmon-test-pg-"));
    assert!(name2.starts_with("meshmon-test-pg-"));

    // Cleanup — leave the host pristine.
    let _ = xtask(&["test-db", "down", "--name", &name1]);
    let _ = xtask(&["test-db", "down", "--name", &name2]);
}

/// `down --name <n>` removes exactly one container; `down` (no flag)
/// reaps every leftover with the prefix.
#[test]
fn xtask_test_db_down_targets_correctly() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }

    let up1 = xtask(&["test-db", "up"]);
    let up2 = xtask(&["test-db", "up"]);
    assert!(
        up1.status.success() && up2.status.success(),
        "ups must succeed"
    );
    let (name1, _) = parse_export_line(&String::from_utf8_lossy(&up1.stdout)).expect("parse up1");
    let (name2, _) = parse_export_line(&String::from_utf8_lossy(&up2.stdout)).expect("parse up2");

    // Targeted down removes only name1.
    let down1 = xtask(&["test-db", "down", "--name", &name1]);
    assert!(
        down1.status.success(),
        "down --name failed: {}",
        String::from_utf8_lossy(&down1.stderr)
    );

    let status = xtask(&["test-db", "status"]);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(!s.contains(&name1), "name1 should be gone; got: {s}");
    assert!(
        s.contains(&name2),
        "name2 should still be running; got: {s}"
    );

    // Bare down reaps the rest.
    let down_all = xtask(&["test-db", "down"]);
    assert!(down_all.status.success(), "bare down failed");
    let status = xtask(&["test-db", "status"]);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        s.contains("no containers running"),
        "all containers must be reaped; got: {s}"
    );
}

/// `xtask test` runs nextest end-to-end against a fresh container and
/// tears it down on success. This is the integration-level smoke that
/// the prior single-container lifecycle test used to provide.
#[test]
fn xtask_test_runs_nextest_end_to_end() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }
    let have_nextest = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_nextest {
        eprintln!("skip: cargo-nextest not installed");
        return;
    }

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

    // Container should be torn down on the success path.
    let status = xtask(&["test-db", "status"]);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(
        s.contains("no containers running"),
        "test must clean up; got: {s}"
    );
}
