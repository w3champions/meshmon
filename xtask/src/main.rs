//! Maintenance commands for the meshmon workspace.
//!
//! Invoke via the cargo alias defined in `.cargo/config.toml`:
//!
//! ```text
//! cargo xtask <subcommand>
//! ```
//!
//! # Subcommands
//!
//! ## `openapi`
//!
//! Regenerate `frontend/src/api/openapi.gen.json` from the service's
//! compile-time OpenAPI schema. The frontend build reads this file to
//! generate TS types and a typed fetch client; CI fails if the
//! checked-in copy diverges from what `cargo xtask openapi` produces.
//!
//! The `.gen.` infix marks the file as build-artifact output so tooling
//! (lint ignores, codeowners, review heuristics) can filter every
//! generated file via a single `**/*.gen.*` glob.
//!
//! Sort order: the emitted JSON is serialized with `serde_json::to_string_pretty`
//! and ends with a trailing newline. Deterministic output keeps `git diff`
//! clean across regenerations.
//!
//! ## `test-db <up|down|status>`
//!
//! Manage a shared TimescaleDB container for integration tests.
//! See `src/test_db.rs` for details.
//!
//! ## `test [-- <nextest-args>]`
//!
//! Provision the shared DB and run the full workspace test suite via
//! `cargo nextest`. Requires `cargo-nextest` to be installed.
//!
//! ## `test-e2e [-- <cargo-test-args>]`
//!
//! Bring up the compose stack and run the `meshmon-e2e` test package.

mod signal;
mod test_cmd;
mod test_db;

use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::path::PathBuf;

const OPENAPI_RELATIVE_PATH: &str = "frontend/src/api/openapi.gen.json";

fn main() -> Result<()> {
    signal::install_once();
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        print_usage();
        bail!("no subcommand given");
    };
    match cmd.as_str() {
        "openapi" => cmd_openapi(),
        "test-db" => {
            let subcmd = args.next().unwrap_or_default();
            match subcmd.as_str() {
                "up" => test_db::up(),
                "down" => test_db::down(),
                "status" => test_db::status(),
                other => {
                    eprintln!("usage: cargo xtask test-db <up|down|status>");
                    bail!("unknown test-db subcommand: {other}");
                }
            }
        }
        "test" => {
            // Collect everything after an optional `--` separator as extra
            // args forwarded to nextest.
            let extra: Vec<String> = args.collect();
            let extra = strip_separator(extra);
            test_cmd::test(extra)
        }
        "test-e2e" => {
            let extra: Vec<String> = args.collect();
            let extra = strip_separator(extra);
            test_cmd::test_e2e(extra)
        }
        other => {
            print_usage();
            bail!("unknown subcommand: {other}");
        }
    }
}

/// Strip a leading `--` separator from extra args, if present.
fn strip_separator(mut args: Vec<String>) -> Vec<String> {
    if args.first().map(|s| s.as_str()) == Some("--") {
        args.remove(0);
    }
    args
}

fn cmd_openapi() -> Result<()> {
    let doc = meshmon_service::http::openapi_document();
    let mut json = serde_json::to_string_pretty(&doc).context("serialize OpenAPI document")?;
    json.push('\n');

    let dest = workspace_root()?.join(OPENAPI_RELATIVE_PATH);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&dest, json.as_bytes()).with_context(|| format!("write {}", dest.display()))?;
    eprintln!("wrote {}", dest.display());
    Ok(())
}

/// Resolve the workspace root by walking up from `CARGO_MANIFEST_DIR` until
/// we find a `Cargo.toml` containing `[workspace]`.
///
/// Exposed `pub(crate)` so subcommand modules (e.g. `test_cmd`) can pin
/// CWD to the workspace root before shelling out — lets `cargo xtask
/// test-e2e` run from any subdirectory.
pub(crate) fn workspace_root() -> Result<PathBuf> {
    let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut dir = start.clone();
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            let text = fs::read_to_string(&candidate)?;
            if text.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            bail!(
                "could not locate workspace root (walked up from {})",
                start.display()
            );
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  openapi              regenerate {OPENAPI_RELATIVE_PATH}");
    eprintln!("  test-db up           start shared TimescaleDB container");
    eprintln!("  test-db down         stop and remove shared TimescaleDB container");
    eprintln!("  test-db status       report container state and DATABASE_URL");
    eprintln!("  test [-- <args>]     provision DB + run workspace tests via nextest");
    eprintln!("  test-e2e [-- <args>] bring up compose stack + run meshmon-e2e");
}
