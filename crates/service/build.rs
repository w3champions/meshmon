//! Build script: emits `MESHMON_GIT_COMMIT` so the release binary can
//! report which commit it was built from via
//! `meshmon_service_build_info{commit=...}`. Falls back to `"unknown"`
//! when git is unavailable (source tarball builds, CI without checkout
//! depth, detached archives) — consistent with the
//! `BuildInfo::compile_time` default.
//!
//! ## Worktree awareness
//!
//! Meshmon development happens inside git worktrees (see sibling dirs
//! under `meshmon/`), so the workspace-root `.git` is a plain *file*
//! containing `gitdir: <admin-dir>`, not a directory. We handle three
//! layouts for `cargo:rerun-if-changed`:
//!
//! 1. `.git` is a directory (plain clone) → watch `.git/HEAD` and
//!    `.git/refs/heads`.
//! 2. `.git` is a file (worktree) → resolve the admin dir and watch
//!    `<admin>/HEAD` (branch pointer for this worktree) plus the shared
//!    `refs/heads` via `<admin>/commondir` (or `<admin>/refs/heads` as
//!    a fallback).
//! 3. No `.git` at all (tarball build) → emit no rerun hints and skip
//!    the `git rev-parse` subprocess entirely so we record `"unknown"`
//!    instead of an unrelated outer-repo hash. Relying on the
//!    subprocess's failure path would let `git` walk the directory
//!    tree upward from the build CWD and embed a neighboring
//!    repository's `HEAD` into `MESHMON_GIT_COMMIT`.
//!
//! When we *do* run `git rev-parse`, we pin it to the resolved
//! workspace via `-C <workspace-root>` so the same upward-walk can't
//! contaminate the answer even on a plain clone layout.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Workspace root is two levels above the crate (`crates/service/`).
    let workspace_root = manifest_dir.join("../../");
    let workspace_git = workspace_root.join(".git");
    let git_resolvable = emit_git_rerun_hints(&workspace_git);

    // Only invoke `git` when we've confirmed a `.git` entry belongs to
    // THIS workspace; otherwise skip to the `"unknown"` fallback. This
    // keeps tarball/detached-source builds honest (no hash from a
    // parent repo that happens to enclose the source tree) while still
    // letting plain clones and worktree layouts emit a real commit.
    let sha = if git_resolvable {
        Command::new("git")
            .arg("-C")
            .arg(&workspace_root)
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_owned())
    } else {
        "unknown".to_owned()
    };

    println!("cargo:rustc-env=MESHMON_GIT_COMMIT={sha}");
}

/// Emit `cargo:rerun-if-changed` hints covering plain clones and git
/// worktrees. Returns `true` when a `.git` entry that belongs to the
/// workspace was successfully resolved — callers gate the
/// `git rev-parse` subprocess on this so tarball builds don't accidentally
/// inherit a parent repository's commit.
fn emit_git_rerun_hints(workspace_git: &Path) -> bool {
    // Resolve (head_path, refs_heads_path) for the three layouts, or
    // `None` to skip emission entirely (tarball / no `.git`).
    let paths = if workspace_git.is_dir() {
        // Plain clone: HEAD and refs/heads live right here.
        Some((workspace_git.join("HEAD"), workspace_git.join("refs/heads")))
    } else if workspace_git.is_file() {
        // Worktree: `.git` is a pointer file `gitdir: <admin-dir>`.
        // The admin dir holds this worktree's HEAD; refs/heads lives
        // in the shared (common) git dir, referenced by
        // `<admin>/commondir` (relative to the admin dir).
        parse_gitdir_pointer(workspace_git).map(|admin_dir| {
            (
                admin_dir.join("HEAD"),
                resolve_shared_refs_heads(&admin_dir),
            )
        })
    } else {
        // No `.git` entry: tarball / detached source build.
        None
    };

    if let Some((head, refs_heads)) = paths {
        println!("cargo:rerun-if-changed={}", head.display());
        println!("cargo:rerun-if-changed={}", refs_heads.display());
        true
    } else {
        false
    }
}

/// Parse the `gitdir: <path>` pointer inside a worktree's `.git` file
/// and return the resolved admin directory path. Relative pointers are
/// anchored to the directory containing the pointer file, matching
/// `git`'s own resolution rules.
fn parse_gitdir_pointer(pointer_file: &Path) -> Option<PathBuf> {
    let contents = fs::read_to_string(pointer_file).ok()?;
    let raw = contents.lines().find_map(|l| l.strip_prefix("gitdir:"))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = PathBuf::from(trimmed);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        pointer_file.parent()?.join(candidate)
    };
    Some(resolved)
}

/// Locate `refs/heads` for a worktree admin dir. Prefers the shared
/// common dir advertised via `<admin>/commondir` (path relative to the
/// admin dir), and falls back to `<admin>/refs/heads` when that file
/// is absent (unusual layouts, older git versions).
fn resolve_shared_refs_heads(admin_dir: &Path) -> PathBuf {
    let commondir_file = admin_dir.join("commondir");
    if let Ok(contents) = fs::read_to_string(&commondir_file) {
        let raw = contents.trim();
        if !raw.is_empty() {
            let candidate = PathBuf::from(raw);
            let shared = if candidate.is_absolute() {
                candidate
            } else {
                admin_dir.join(candidate)
            };
            return shared.join("refs/heads");
        }
    }
    admin_dir.join("refs/heads")
}
