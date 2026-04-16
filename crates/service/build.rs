//! Build script: emits `MESHMON_GIT_COMMIT` so the release binary can
//! report which commit it was built from via
//! `meshmon_service_build_info{commit=...}`. Falls back to `"unknown"`
//! when git is unavailable (source tarball builds, CI without checkout
//! depth) — consistent with the `BuildInfo::compile_time` default.

use std::process::Command;

fn main() {
    // Paths are relative to the crate root; workspace `.git` lives two
    // levels up. Touching a branch ref (commit) triggers a rebuild.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=MESHMON_GIT_COMMIT={sha}");
}
