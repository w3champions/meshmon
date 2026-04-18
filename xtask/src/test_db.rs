//! `cargo xtask test-db` — shared TimescaleDB container lifecycle.
//!
//! KEEP IN SYNC with `crates/service/tests/common/mod.rs::TIMESCALEDB_{IMAGE,TAG}`
//! and `deploy/docker-compose.yml::meshmon-db.image`. If the pair
//! drifts, CI (which calls `xtask test-db up`) and the integration-test
//! harness (which shares the server the test binary assumes) will see
//! different Postgres majors and one of them will break.
//!
//! Consider extracting to a shared crate (e.g., `crates/common::test_db`)
//! if the duplication becomes painful — out of scope here.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const IMAGE: &str = "timescale/timescaledb:2.26.3-pg16";
const CONTAINER_NAME: &str = "meshmon-test-pg";
const HOST_PORT: u16 = 5432;
pub(crate) const DATABASE_URL: &str = "postgres://postgres:postgres@localhost:5432/postgres";

pub fn up() -> anyhow::Result<()> {
    if is_running()? {
        println!("test DB already running on port {HOST_PORT}");
    } else {
        docker_run()?;
        wait_ready(Duration::from_secs(30))?;
        println!("started {CONTAINER_NAME} ({IMAGE})");
    }
    println!();
    println!("export DATABASE_URL={DATABASE_URL}");
    Ok(())
}

pub fn down() -> anyhow::Result<()> {
    // `docker rm -f` succeeds when the container exists and is removed.
    // When the container is absent, exit codes vary across Docker
    // versions (Docker >= 25 started returning non-zero for missing
    // names), so this command is NOT universally idempotent at the
    // exit-code level. The business-level contract is still idempotent:
    // callers expect `down` to leave no container behind and the next
    // `up` to succeed. On a noisy-missing exit we bail; any truly
    // leftover container would then fail the next `up` visibly.
    let status = Command::new("docker")
        .args(["rm", "-f", CONTAINER_NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        println!("removed {CONTAINER_NAME} (or it was already absent)");
        Ok(())
    } else {
        anyhow::bail!("docker rm -f {CONTAINER_NAME} failed ({status})")
    }
}

pub fn status() -> anyhow::Result<()> {
    if is_running()? {
        println!("running on localhost:{HOST_PORT}");
        println!("DATABASE_URL={DATABASE_URL}");
    } else {
        println!("not running");
    }
    Ok(())
}

fn is_running() -> anyhow::Result<bool> {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", CONTAINER_NAME])
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Ok(false);
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "true")
}

fn docker_run() -> anyhow::Result<()> {
    let status = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            CONTAINER_NAME,
            "-e",
            "POSTGRES_PASSWORD=postgres",
            "-e",
            "POSTGRES_USER=postgres",
            "-e",
            "POSTGRES_DB=postgres",
            "-p",
            &format!("{HOST_PORT}:5432"),
            IMAGE,
        ])
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "docker run for {CONTAINER_NAME} failed ({status}); \
             check that port {HOST_PORT} is free and Docker is running"
        );
    }
    Ok(())
}

fn wait_ready(timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        let out = Command::new("docker")
            .args(["exec", CONTAINER_NAME, "pg_isready", "-U", "postgres"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            anyhow::bail!("{CONTAINER_NAME} did not become ready within {:?}", timeout);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
