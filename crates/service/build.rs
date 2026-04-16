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
//! 3. No `.git` at all (tarball build) → emit no rerun hints; the
//!    `git rev-parse` subprocess fails and we record `"unknown"`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Workspace root is two levels above the crate (`crates/service/`).
    let workspace_git = manifest_dir.join("../../.git");
    emit_git_rerun_hints(&workspace_git);

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

/// Emit `cargo:rerun-if-changed` hints covering plain clones and git
/// worktrees. Silently returns when no `.git` entry exists so tarball
/// builds don't fail — the `git rev-parse` fallback handles those.
fn emit_git_rerun_hints(workspace_git: &Path) {
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
