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

fn sqlx_cli_available() -> bool {
    Command::new("sqlx")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `xtask sqlx-prepare -- --check` runs end-to-end against a throwaway
/// container and tears it down on exit. We use `--check` so the test
/// asserts cache freshness without modifying the working tree.
///
/// Skips when sqlx-cli or docker is missing — same posture as the
/// test-db lifecycle test.
#[test]
fn xtask_sqlx_prepare_check_runs_end_to_end() {
    if !docker_available() {
        eprintln!("skip: docker unavailable");
        return;
    }
    if !sqlx_cli_available() {
        eprintln!("skip: sqlx-cli not installed");
        return;
    }

    // Snapshot pre-existing containers — the assertion is "no NEW
    // sqlx-prep containers leaked", not "zero exist now". A parallel
    // dev workflow may have its own sqlx-prep container running.
    let before = list_sqlx_prep_containers();

    let out = xtask(&["sqlx-prepare", "--", "--check"]);
    assert!(
        out.status.success(),
        ".sqlx/ cache is stale or sqlx-prepare crashed; stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = list_sqlx_prep_containers();
    let leaked: Vec<&String> = after.iter().filter(|n| !before.contains(n)).collect();
    assert!(
        leaked.is_empty(),
        "sqlx-prepare leaked containers: {leaked:?}"
    );
}

fn list_sqlx_prep_containers() -> Vec<String> {
    let out = Command::new("docker")
        .args(["ps", "-a", "--format", "{{.Names}}"])
        .output()
        .expect("docker ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|n| n.starts_with("meshmon-sqlx-prep-"))
        .collect()
}
