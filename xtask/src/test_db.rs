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
    match container_state()? {
        ContainerState::Running => {
            println!("test DB already running on port {HOST_PORT}");
        }
        ContainerState::Stopped => {
            docker_start()?;
            wait_ready(Duration::from_secs(30))?;
            println!("started existing {CONTAINER_NAME} ({IMAGE})");
        }
        ContainerState::Absent => {
            docker_run()?;
            wait_ready(Duration::from_secs(30))?;
            println!("created {CONTAINER_NAME} ({IMAGE})");
        }
    }
    println!();
    println!("export DATABASE_URL={DATABASE_URL}");
    Ok(())
}

pub fn down() -> anyhow::Result<()> {
    // Idempotent at both the business and exit-code level: on Docker >=
    // 25, `docker rm -f <name>` returns non-zero when the container is
    // absent. Pre-check via `container_state()` so that repeated calls
    // (e.g. a CI teardown with `if: always()` plus a developer running
    // `down` twice) all exit 0.
    if matches!(container_state()?, ContainerState::Absent) {
        println!("{CONTAINER_NAME} already absent");
        return Ok(());
    }
    let status = Command::new("docker")
        .args(["rm", "-f", CONTAINER_NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        println!("removed {CONTAINER_NAME}");
        Ok(())
    } else {
        anyhow::bail!("docker rm -f {CONTAINER_NAME} failed ({status})")
    }
}

pub fn status() -> anyhow::Result<()> {
    match container_state()? {
        ContainerState::Running => {
            println!("running on localhost:{HOST_PORT}");
            println!("DATABASE_URL={DATABASE_URL}");
        }
        ContainerState::Stopped | ContainerState::Absent => {
            println!("not running");
        }
    }
    Ok(())
}

/// Three-way state of the shared test DB container.
///
/// `Absent` vs `Stopped` matters because `docker run --name X` on a
/// stopped-but-existing container fails with "container name already in
/// use" — the `up()` path must `docker start X` instead.
enum ContainerState {
    Running,
    Stopped,
    Absent,
}

fn container_state() -> anyhow::Result<ContainerState> {
    let out = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Running}}", CONTAINER_NAME])
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        // `docker inspect` exits non-zero when the container doesn't exist.
        return Ok(ContainerState::Absent);
    }
    let running = String::from_utf8_lossy(&out.stdout).trim() == "true";
    Ok(if running {
        ContainerState::Running
    } else {
        ContainerState::Stopped
    })
}

fn docker_start() -> anyhow::Result<()> {
    let status = Command::new("docker")
        .args(["start", CONTAINER_NAME])
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        anyhow::bail!("docker start {CONTAINER_NAME} failed ({status})");
    }
    Ok(())
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
