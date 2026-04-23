//! `cargo xtask test-db` — per-invocation TimescaleDB containers.
//!
//! KEEP IN SYNC with `crates/service/tests/common/mod.rs::TIMESCALEDB_{IMAGE,TAG}`
//! and `deploy/docker-compose.yml::meshmon-db.image`. If the pair drifts,
//! CI (which calls `xtask test`) and the integration-test harness will
//! talk to different Postgres majors and one of them will break.
//!
//! Container naming: every `up` call mints a fresh
//! `meshmon-test-pg-<short-uuid>` container with `-p 0:5432`, so
//! parallel `xtask test` runs (developer + CI, multiple terminals,
//! pre-commit hooks) never collide on either name or host port. The
//! `up_unique()` helper returns a [`TestDbContainer`] RAII guard whose
//! `Drop` runs `docker rm -f`; signal handlers (`crate::signal`) ensure
//! the same teardown runs on Ctrl-C.

use crate::signal;
use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub(crate) const TIMESCALEDB_IMAGE: &str = "timescale/timescaledb:2.26.3-pg16";
pub(crate) const TEST_PREFIX: &str = "meshmon-test-pg-";
pub(crate) const SQLX_PREP_PREFIX: &str = "meshmon-sqlx-prep-";

/// RAII handle for one ephemeral Postgres container.
///
/// `Drop` runs `docker rm -f <name>`; a paired entry in the process
/// signal-handler registry triggers the same removal on Ctrl-C. Both
/// paths are idempotent — `docker rm -f` on an absent container is a
/// silent no-op via stderr discard.
pub struct TestDbContainer {
    name: String,
    port: u16,
    /// Set to `false` by [`Self::leak`] when the caller wants the
    /// container to outlive this guard (used by `xtask test-db up`,
    /// which transfers ownership to the user).
    teardown_on_drop: Arc<Mutex<bool>>,
}

impl TestDbContainer {
    /// Container name (e.g. `meshmon-test-pg-1a2b3c4d`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Host port assigned by the kernel.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// `postgres://postgres:postgres@127.0.0.1:<port>/postgres`. The
    /// `postgres/postgres` credentials are baked in by `docker_run_unique`
    /// and shared between `up_unique` and `up_sqlx_prep_unique` containers
    /// so this method is correct for both.
    pub fn database_url(&self) -> String {
        format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            self.port
        )
    }

    /// Detach the guard so the container survives `Drop`. Used by
    /// `xtask test-db up`, which prints the export line and hands the
    /// container off to the caller.
    pub fn leak(self) -> (String, u16) {
        *self.teardown_on_drop.lock().expect("poison") = false;
        let name = self.name.clone();
        let port = self.port;
        // Drop runs but the flag tells it to no-op.
        drop(self);
        (name, port)
    }
}

impl Drop for TestDbContainer {
    fn drop(&mut self) {
        let should_tear_down = *self.teardown_on_drop.lock().expect("poison");
        if !should_tear_down {
            return;
        }
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Spawn a fresh `meshmon-test-pg-<uuid>` container and return its
/// RAII guard. Blocks until `pg_isready` succeeds (30 s budget).
///
/// Registers a signal-handler teardown so Ctrl-C also reaps the
/// container. The teardown reads the same name as `Drop` does, so a
/// race between Ctrl-C and normal exit can't double-remove (docker
/// silently no-ops the second call).
pub fn up_unique() -> Result<TestDbContainer> {
    let uuid = Uuid::new_v4().simple().to_string();
    let name = format!("{TEST_PREFIX}{}", &uuid[..8]);
    docker_run_unique(&name, TIMESCALEDB_IMAGE, "postgres", "postgres")?;
    finalize_or_cleanup(name, |n| {
        let port = inspect_port(n)?;
        wait_ready(n, Duration::from_secs(30))?;
        Ok(port)
    })
}

/// Spawn a `meshmon-sqlx-prep-<uuid>` container with the same shape as
/// the test DB and using the same `postgres` credentials so
/// `TestDbContainer::database_url()` is correct for both container types.
pub fn up_sqlx_prep_unique() -> Result<TestDbContainer> {
    let uuid = Uuid::new_v4().simple().to_string();
    let name = format!("{SQLX_PREP_PREFIX}{}", &uuid[..8]);
    docker_run_unique(&name, TIMESCALEDB_IMAGE, "postgres", "postgres")?;
    finalize_or_cleanup(name, |n| {
        let port = inspect_port(n)?;
        wait_ready(n, Duration::from_secs(30))?;
        Ok(port)
    })
}

/// Run post-`docker run` setup (port discovery + readiness wait); on
/// failure, best-effort `docker rm -f` the container so a startup
/// failure (image pull stall, daemon hiccup, slow CI) doesn't leak it.
/// The signal-teardown handler is registered first, so Ctrl-C during
/// the wait_ready window also reaps the container.
fn finalize_or_cleanup(
    name: String,
    setup: impl FnOnce(&str) -> Result<u16>,
) -> Result<TestDbContainer> {
    register_signal_teardown(&name);
    match setup(&name) {
        Ok(port) => Ok(TestDbContainer {
            name,
            port,
            teardown_on_drop: Arc::new(Mutex::new(true)),
        }),
        Err(e) => {
            let _ = Command::new("docker")
                .args(["rm", "-f", &name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            Err(e)
        }
    }
}

/// `cargo xtask test-db up` entry point. Spawns a fresh container,
/// prints the export line + container name, and detaches — the caller
/// is responsible for calling `xtask test-db down --name <name>`
/// (or the bare `down` to reap every leftover).
pub fn cmd_up() -> Result<()> {
    let guard = up_unique()?;
    let url = guard.database_url();
    let (name, port) = guard.leak();
    println!("created {name} on host port {port}");
    println!();
    println!("export DATABASE_URL={url}");
    println!("# tear down with: cargo xtask test-db down --name {name}");
    Ok(())
}

/// `cargo xtask test-db down [--name <n>]`. With no name, removes
/// every `meshmon-test-pg-*` and `meshmon-sqlx-prep-*` container
/// (idempotent).
pub fn cmd_down(name: Option<String>) -> Result<()> {
    match name {
        Some(n) => down_one(&n),
        None => {
            down_all_prefix(TEST_PREFIX)?;
            down_all_prefix(SQLX_PREP_PREFIX)
        }
    }
}

/// `cargo xtask test-db status`. Lists every running container with
/// the `meshmon-test-pg-` or `meshmon-sqlx-prep-` prefix and the
/// connect URL.
pub fn cmd_status() -> Result<()> {
    let mut names = list_running_with_prefix(TEST_PREFIX)?;
    names.extend(list_running_with_prefix(SQLX_PREP_PREFIX)?);
    if names.is_empty() {
        println!("no containers running");
        return Ok(());
    }
    for name in &names {
        match inspect_port(name) {
            Ok(port) => {
                println!(
                    "{name}  127.0.0.1:{port}  postgres://postgres:postgres@127.0.0.1:{port}/postgres"
                );
            }
            Err(e) => {
                println!("{name}  (port unknown: {e})");
            }
        }
    }
    Ok(())
}

fn down_one(name: &str) -> Result<()> {
    let status = Command::new("docker")
        .args(["rm", "-f", name])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if status.status.success() {
        println!("removed {name}");
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&status.stderr);
        if err.to_ascii_lowercase().contains("no such") {
            println!("{name} already absent");
            Ok(())
        } else {
            bail!("docker rm -f {name} failed: {}", err.trim())
        }
    }
}

fn down_all_prefix(prefix: &str) -> Result<()> {
    let names = list_all_with_prefix(prefix)?;
    if names.is_empty() {
        println!("no containers found with prefix '{prefix}'");
        return Ok(());
    }
    for name in &names {
        down_one(name)?;
    }
    Ok(())
}

fn docker_run_unique(name: &str, image: &str, user: &str, password: &str) -> Result<()> {
    let status = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            name,
            "-e",
            &format!("POSTGRES_USER={user}"),
            "-e",
            &format!("POSTGRES_PASSWORD={password}"),
            "-e",
            "POSTGRES_DB=postgres",
            "-p",
            "127.0.0.1::5432",
            image,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("spawn docker for {name}"))?;
    if !status.status.success() {
        bail!(
            "docker run for {name} failed ({}): {}",
            status.status,
            String::from_utf8_lossy(&status.stderr).trim()
        );
    }
    Ok(())
}

/// Resolve the host port docker assigned to container port 5432/tcp.
fn inspect_port(name: &str) -> Result<u16> {
    // `docker port <name> 5432/tcp` prints lines like
    //   127.0.0.1:54321
    //   [::]:54321
    // Take the first one and parse the trailing port number. We do
    // not use `docker inspect -f '...'` here because the JSON path
    // for HostPort is awkward and varies by daemon version.
    let out = Command::new("docker")
        .args(["port", name, "5432/tcp"])
        .output()
        .with_context(|| format!("docker port {name}"))?;
    if !out.status.success() {
        bail!(
            "docker port {name} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .next()
        .ok_or_else(|| anyhow::anyhow!("docker port {name} returned no lines"))?;
    let port_str = line
        .rsplit(':')
        .next()
        .ok_or_else(|| anyhow::anyhow!("could not parse port from '{line}'"))?;
    port_str
        .trim()
        .parse::<u16>()
        .with_context(|| format!("parse host port from '{line}'"))
}

fn list_running_with_prefix(prefix: &str) -> Result<Vec<String>> {
    list_with_prefix(prefix, &["ps", "--format", "{{.Names}}"])
}

fn list_all_with_prefix(prefix: &str) -> Result<Vec<String>> {
    list_with_prefix(prefix, &["ps", "-a", "--format", "{{.Names}}"])
}

fn list_with_prefix(prefix: &str, args: &[&str]) -> Result<Vec<String>> {
    let out = Command::new("docker")
        .args(args)
        .output()
        .context("docker ps")?;
    if !out.status.success() {
        bail!(
            "docker ps failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|n| n.starts_with(prefix))
        .collect())
}

fn wait_ready(name: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let out = Command::new("docker")
            .args(["exec", name, "pg_isready", "-U", "postgres"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            bail!("{name} did not become ready within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn register_signal_teardown(name: &str) {
    let owned = name.to_string();
    signal::on_signal(move || {
        let _ = Command::new("docker")
            .args(["rm", "-f", &owned])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}
