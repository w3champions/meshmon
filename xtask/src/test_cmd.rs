//! `cargo xtask test` and `cargo xtask test-e2e`.

use std::process::Command;

use crate::test_db;

pub fn test(extra: Vec<String>) -> anyhow::Result<()> {
    // Spawn a fresh per-invocation container. The guard's Drop runs
    // `docker rm -f` on every exit path; signal::on_signal (registered
    // inside up_unique) covers Ctrl-C.
    let db = test_db::up_unique()?;
    let database_url = db.database_url();

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

    // Exclusions:
    //   * `meshmon-e2e` — requires the full compose stack, covered by
    //     `cargo xtask test-e2e`.
    //   * `xtask` — its integration tests drive container lifecycles
    //     (up/down/inspect) directly. Letting them race against an
    //     in-flight `xtask test` invocation muddies the docker-side
    //     state. Run them via `cargo test -p xtask`.
    let mut cmd = Command::new("cargo");
    cmd.args([
        "nextest",
        "run",
        "--workspace",
        "--exclude",
        "meshmon-e2e",
        "--exclude",
        "xtask",
        "--all-targets",
    ])
    .args(&extra)
    .env("DATABASE_URL", &database_url)
    // Use the committed .sqlx/ offline cache so sqlx::query! macros do
    // not try to verify queries against the just-provisioned (un-migrated)
    // DB during compilation.
    .env("SQLX_OFFLINE", "true");
    let status = cmd.status()?;
    if !status.success() {
        // `db` drops on the std::process::exit path? No — exit() does
        // not unwind. Force teardown explicitly before exiting.
        drop(db);
        std::process::exit(status.code().unwrap_or(1));
    }

    // Doctests: nextest doesn't cover them. Run separately if any exist.
    // (A single pass across the workspace; cheap when there are none.)
    // Same exclusions as above for the same reasons.
    let status = Command::new("cargo")
        .args([
            "test",
            "--doc",
            "--workspace",
            "--exclude",
            "meshmon-e2e",
            "--exclude",
            "xtask",
        ])
        .env("DATABASE_URL", &database_url)
        .env("SQLX_OFFLINE", "true")
        .status()?;
    if !status.success() {
        drop(db);
        std::process::exit(status.code().unwrap_or(1));
    }

    // Explicit drop is redundant on the success path (Rust would do it
    // anyway), but documents intent — the container teardown is part of
    // the contract, not a side effect.
    drop(db);
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
    let mut cmd = Command::new("docker");
    cmd.current_dir(&root)
        .arg("compose")
        .args(&compose_args)
        .args(["up", "-d", "--build", "--wait"]);

    // `COMPOSE_BAKE=true` is required by the CI cache overlay
    // (`cache_to: type=gha`) but forces a minimum Compose v2.34. Turning
    // it on unconditionally breaks local dev on older Compose even when
    // the overlay isn't loaded — so only enable it when the overlay is
    // actually in use (i.e. in CI via MESHMON_E2E_CACHE_OVERLAY).
    if overlay.is_some() {
        cmd.env("COMPOSE_BAKE", "true");
    }

    let status = cmd.status()?;
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
