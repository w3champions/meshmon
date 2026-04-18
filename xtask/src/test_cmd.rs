//! `cargo xtask test` and `cargo xtask test-e2e`.

use std::process::Command;

use crate::test_db;

pub fn test(extra: Vec<String>) -> anyhow::Result<()> {
    test_db::up()?;

    // nextest must be installed — xtask deliberately does NOT install
    // it (cargo installs are slow in CI; CI installs via
    // taiki-e/install-action@nextest, locally use `cargo install
    // cargo-nextest --locked` or `brew install cargo-nextest`).
    let have_nextest = Command::new("cargo")
        .args(["nextest", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !have_nextest {
        anyhow::bail!(
            "cargo-nextest not found. Install via:\n  \
             cargo install cargo-nextest --locked\n  \
             or: brew install cargo-nextest"
        );
    }

    let mut cmd = Command::new("cargo");
    cmd.args([
        "nextest",
        "run",
        "--workspace",
        "--exclude",
        "meshmon-e2e",
        "--all-targets",
    ])
    .args(&extra)
    .env("DATABASE_URL", test_db::DATABASE_URL)
    // Use the committed .sqlx/ offline cache so sqlx::query! macros do
    // not try to verify queries against the just-provisioned (un-migrated)
    // DB during compilation.
    .env("SQLX_OFFLINE", "true");
    let status = cmd.status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    // Doctests: nextest doesn't cover them. Run separately if any exist.
    // (A single pass across the workspace; cheap when there are none.)
    let status = Command::new("cargo")
        .args(["test", "--doc", "--workspace", "--exclude", "meshmon-e2e"])
        .env("DATABASE_URL", test_db::DATABASE_URL)
        .env("SQLX_OFFLINE", "true")
        .status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
}

pub fn test_e2e(extra: Vec<String>) -> anyhow::Result<()> {
    // Pin CWD to the workspace root so relative compose paths resolve
    // regardless of where the user invoked `cargo xtask test-e2e`. A
    // developer iterating inside `crates/service` would otherwise see
    // docker compose fail on "no such file or directory".
    let root = crate::workspace_root()?;

    // Base compose file is the local-dev-safe default. CI sets
    // `MESHMON_E2E_CACHE_OVERLAY=deploy/docker-compose.ci-cache.yml`
    // to layer GHA cache backends on top — that overlay requires
    // ACTIONS_RUNTIME_TOKEN and must not be included locally.
    let base = "deploy/docker-compose.yml";
    let overlay = std::env::var("MESHMON_E2E_CACHE_OVERLAY").ok();

    let mut compose_args: Vec<String> = vec!["-f".into(), base.into()];
    if let Some(path) = overlay.as_deref() {
        compose_args.push("-f".into());
        compose_args.push(path.into());
    }

    // Bring stack up (idempotent: `up` reuses running services).
    let status = Command::new("docker")
        .current_dir(&root)
        .arg("compose")
        .args(&compose_args)
        .args(["up", "-d", "--build", "--wait"])
        .env("COMPOSE_BAKE", "true")
        .status()?;
    if !status.success() {
        anyhow::bail!("docker compose up failed; check deploy/.env is staged");
    }

    wait_for_readyz(std::time::Duration::from_secs(30))?;

    let status = Command::new("cargo")
        .current_dir(&root)
        .args(["test", "-p", "meshmon-e2e"])
        .args(&extra)
        .status()?;
    // Compose stack is intentionally left UP — re-runs are fast. Teardown:
    // `docker compose -f deploy/docker-compose.yml down -v`.
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn wait_for_readyz(timeout: std::time::Duration) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let ok = Command::new("curl")
            .args([
                "-fsS",
                "--max-time",
                "2",
                "-o",
                "/dev/null",
                "http://localhost:8080/readyz",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "meshmon-service /readyz did not respond within {:?}",
                timeout
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
