//! `cargo xtask test` and `cargo xtask test-e2e`.

use std::process::Command;

use crate::test_db;

/// Strip the per-package `CARGO_*` env vars that the outer `cargo run
/// -p xtask` leaks into our process. Inheriting them into a freshly
/// spawned `cargo` causes its build-script fingerprints to diff against
/// any shell-invoked `cargo` run — every alternation between `cargo
/// xtask test` and a plain `cargo build` would otherwise rebuild
/// build-scripts that read `CARGO_MANIFEST_DIR` (ring, rustls,
/// sqlx-macros-core, …) and cascade through their reverse-deps. See
/// `cargo::core::compiler::fingerprint` `EnvVarChanged` traces.
///
/// Preserves user-facing config (`CARGO_HOME`, `CARGO_TARGET_DIR`,
/// `CARGO_NET_*`, `CARGO_HTTP_*`, `CARGO_BUILD_*`, `CARGO_PROFILE_*`,
/// `CARGO_REGISTRIES_*`, `CARGO_TERM_*`) so explicit shell overrides
/// still flow through to the spawned cargo.
fn scrub_outer_cargo_env(cmd: &mut Command) {
    // Exact-match leaks set by cargo per-package while building xtask.
    const EXACT: &[&str] = &[
        "CARGO",
        "CARGO_MANIFEST_DIR",
        "CARGO_MANIFEST_LINKS",
        "CARGO_MANIFEST_PATH",
        "CARGO_PRIMARY_PACKAGE",
        "CARGO_BIN_NAME",
        "CARGO_CRATE_NAME",
        "CARGO_RUSTC_CURRENT_DIR",
        "CARGO_TARGET_TMPDIR",
    ];
    for key in EXACT {
        cmd.env_remove(key);
    }
    // Prefix-match leaks: per-package metadata (`CARGO_PKG_*`),
    // build-script cfg values (`CARGO_CFG_*`), feature flags
    // (`CARGO_FEATURE_*`), and dependency metadata (`CARGO_DEP_*`).
    for (key, _) in std::env::vars_os() {
        let Some(s) = key.to_str() else { continue };
        if s.starts_with("CARGO_PKG_")
            || s.starts_with("CARGO_CFG_")
            || s.starts_with("CARGO_FEATURE_")
            || s.starts_with("CARGO_DEP_")
            || s.starts_with("CARGO_BIN_EXE_")
        {
            cmd.env_remove(s);
        }
    }
}

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
    //
    // Target/package narrowing: cargo target-selection flags are unioned,
    // so `--all-targets` would absorb any user-supplied `--test foo` /
    // `--lib` / `--bin foo`. Drop `--all-targets` when the user passes
    // one. Likewise drop `--workspace` (and the workspace-level
    // `--exclude` flags it pairs with) when `-p`/`--package` narrows the
    // scope explicitly.
    let user_picks_targets = extra.iter().any(|a| {
        matches!(
            a.as_str(),
            "--lib" | "--bins" | "--tests" | "--benches" | "--examples" | "--test" | "--bin"
        )
    });
    let user_picks_package = extra
        .iter()
        .any(|a| matches!(a.as_str(), "-p" | "--package"));

    let mut cmd = Command::new("cargo");
    scrub_outer_cargo_env(&mut cmd);
    cmd.args(["nextest", "run"]);
    if !user_picks_package {
        cmd.args([
            "--workspace",
            "--exclude",
            "meshmon-e2e",
            "--exclude",
            "xtask",
        ]);
    }
    if !user_picks_targets {
        cmd.arg("--all-targets");
    }
    cmd.args(&extra)
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
    // Doctests don't apply when the user narrowed to a specific test
    // binary (`--test foo`) — skip the pass entirely. Otherwise mirror
    // the package-scope decision from above.
    let status = if user_picks_targets {
        std::process::ExitStatus::default()
    } else {
        let mut doc = Command::new("cargo");
        scrub_outer_cargo_env(&mut doc);
        doc.args(["test", "--doc"]);
        if !user_picks_package {
            doc.args([
                "--workspace",
                "--exclude",
                "meshmon-e2e",
                "--exclude",
                "xtask",
            ]);
        }
        doc.env("DATABASE_URL", &database_url)
            .env("SQLX_OFFLINE", "true")
            .status()?
    };
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

    let mut e2e = Command::new("cargo");
    scrub_outer_cargo_env(&mut e2e);
    let status = e2e
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
