//! `cargo xtask sqlx-prepare` — regenerate the committed `.sqlx/`
//! offline cache against a throwaway TimescaleDB container.
//!
//! Replaces the manual recipe that previously lived in `README.md` and
//! `crates/service/README.md`. Each invocation:
//!
//! 1. Spawns a unique `meshmon-sqlx-prep-<uuid>` container with
//!    `-p 0:5432`, so two `xtask sqlx-prepare` runs (developer +
//!    pre-commit hook) never collide.
//! 2. Waits for `pg_isready`.
//! 3. Runs `sqlx migrate run --source crates/service/migrations`.
//! 4. Runs `cargo sqlx prepare --workspace -- --all-targets --all-features`.
//! 5. Tears the container down (RAII guard + `crate::signal` for
//!    Ctrl-C).
//!
//! Exit code is the prepare exit code; `.sqlx/` is left in the working
//! tree for the developer to inspect and commit.

use crate::test_db;
use anyhow::{bail, Context, Result};
use std::process::Command;

pub fn run(extra: Vec<String>) -> Result<()> {
    if !sqlx_cli_available() {
        bail!(
            "sqlx-cli not found. Install via:\n  \
             cargo install sqlx-cli --no-default-features --features rustls,postgres --version ~0.8"
        );
    }

    let root = crate::workspace_root()?;
    let db = test_db::up_sqlx_prep_unique()?;
    let database_url = format!(
        "postgres://postgres:meshmon@127.0.0.1:{}/postgres",
        db.port()
    );

    eprintln!(
        "[xtask sqlx-prepare] container {name} on host port {port}",
        name = db.name(),
        port = db.port()
    );

    // Run migrations against the throwaway DB.
    let status = Command::new("sqlx")
        .current_dir(&root)
        .args(["migrate", "run", "--source", "crates/service/migrations"])
        .env("DATABASE_URL", &database_url)
        .status()
        .context("invoke sqlx migrate run")?;
    if !status.success() {
        drop(db);
        std::process::exit(status.code().unwrap_or(1));
    }

    // Run `cargo sqlx prepare`. Forwarded extras land after the
    // `--` separator so a developer can pass `-- --check` to verify
    // the cache instead of regenerating it.
    let mut cmd = Command::new("cargo");
    cmd.current_dir(&root)
        .args(["sqlx", "prepare", "--workspace", "--"])
        .args(["--all-targets", "--all-features"])
        .args(&extra)
        .env("DATABASE_URL", &database_url);
    let status = cmd.status().context("invoke cargo sqlx prepare")?;

    let exit_code = status.code().unwrap_or(1);
    drop(db);

    if !status.success() {
        std::process::exit(exit_code);
    }
    eprintln!("[xtask sqlx-prepare] .sqlx/ regenerated; review with `git diff .sqlx/` and commit");
    Ok(())
}

fn sqlx_cli_available() -> bool {
    Command::new("sqlx")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
