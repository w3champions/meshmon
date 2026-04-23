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

fn running_test_pg_containers() -> std::collections::BTreeSet<String> {
    let out = std::process::Command::new("docker")
        .args([
            "ps",
            "--format",
            "{{.Names}}",
            "--filter",
            "name=meshmon-test-pg-",
        ])
        .output()
        .expect("docker ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|n| n.starts_with("meshmon-test-pg-"))
        .collect()
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

/// `down --name <n>` removes exactly one container. We deliberately
/// avoid asserting global state (e.g. bare `down` + "no containers
/// running") because other concurrent test runs or operator workflows
/// on the same Docker daemon may legitimately have their own
/// `meshmon-test-pg-*` containers in flight.
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

    // Targeted down on the second container — proves `--name` reaps
    // exactly the named container without trampling siblings owned by
    // other concurrent runs.
    let down2 = xtask(&["test-db", "down", "--name", &name2]);
    assert!(
        down2.status.success(),
        "down --name failed: {}",
        String::from_utf8_lossy(&down2.stderr)
    );

    let status = xtask(&["test-db", "status"]);
    let s = String::from_utf8_lossy(&status.stdout);
    assert!(!s.contains(&name1), "name1 should still be gone; got: {s}");
    assert!(!s.contains(&name2), "name2 should be gone; got: {s}");
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

    // Snapshot the set of running test-pg containers BEFORE the run
    // so we can prove the test's container was reaped without
    // depending on global state (other concurrent runs may legitimately
    // have their own containers up).
    let before = running_test_pg_containers();

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

    // No NEW meshmon-test-pg-* container should be running after
    // `xtask test` exits — the container it spawned must have been
    // torn down on the success path. We diff against the pre-run
    // snapshot instead of asserting a global "no containers running"
    // state.
    let after = running_test_pg_containers();
    let leaked: Vec<&String> = after.difference(&before).collect();
    assert!(
        leaked.is_empty(),
        "xtask test leaked containers: {leaked:?} (before={before:?}, after={after:?})"
    );
}
