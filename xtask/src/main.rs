//! Maintenance commands for the meshmon workspace.
//!
//! Invoke via the cargo alias defined in `.cargo/config.toml`:
//!
//! ```text
//! cargo xtask <subcommand>
//! ```
//!
//! Subcommands:
//! - `openapi` — regenerate `frontend/src/api/openapi.json` from the service's
//!   compile-time OpenAPI schema. (Implemented in T04 Task 16.)

use anyhow::{bail, Result};
use std::env;

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(cmd) = args.next() else {
        print_usage();
        bail!("no subcommand given");
    };
    match cmd.as_str() {
        "openapi" => bail!("openapi subcommand not yet implemented"),
        other => {
            print_usage();
            bail!("unknown subcommand: {other}");
        }
    }
}

fn print_usage() {
    eprintln!("usage: cargo xtask <subcommand>");
    eprintln!();
    eprintln!("subcommands:");
    eprintln!("  openapi    regenerate frontend/src/api/openapi.json");
}
