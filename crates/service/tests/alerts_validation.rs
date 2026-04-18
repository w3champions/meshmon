//! Integration test: validate alert rule YAML, vmalert-tool unit tests,
//! and Alertmanager config. Self-managed: each check is a `docker run
//! --rm` invocation.
//!
//! Ported from `scripts/validate-alerts.sh` (now deleted).
//!
//! Requires docker to be running locally. If docker is unreachable the
//! test panics with a clear message. Image tags come from
//! `deploy/versions.env` at runtime.

use std::{collections::HashMap, fs, path::PathBuf, process::Command};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Parse `KEY=value` lines from `deploy/versions.env`. Lines starting
/// with `#` and blanks are skipped. Values have no quoting in the
/// committed file so a naive split is sufficient.
fn load_versions() -> HashMap<String, String> {
    let text = fs::read_to_string(repo_root().join("deploy/versions.env"))
        .expect("read deploy/versions.env");
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn tag(versions: &HashMap<String, String>, key: &str) -> String {
    versions
        .get(key)
        .unwrap_or_else(|| panic!("deploy/versions.env missing {key}"))
        .clone()
}

fn check_docker_available() {
    let output = Command::new("docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .output()
        .expect("docker CLI not found on PATH");
    assert!(
        output.status.success(),
        "docker daemon not reachable: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run(cmd: &mut Command, ctx: &str) {
    let output = cmd.output().unwrap_or_else(|e| panic!("spawn {ctx}: {e}"));
    if !output.status.success() {
        panic!(
            "{ctx} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn vmalert_dry_run_accepts_rules_yaml() {
    check_docker_available();
    let root = repo_root();
    let versions = load_versions();
    let vmalert = tag(&versions, "VMALERT_TAG");
    run(
        Command::new("docker").args([
            "run",
            "--rm",
            "-v",
            &format!("{}:/alerts:ro", root.join("deploy/alerts").display()),
            &format!("victoriametrics/vmalert:{vmalert}"),
            "-dryRun",
            "-rule=/alerts/rules.yaml",
        ]),
        "vmalert -dryRun",
    );
}

#[test]
fn vmalert_tool_unittests_pass() {
    check_docker_available();
    let root = repo_root();
    let tests_dir = root.join("deploy/alerts/tests");
    if !tests_dir.exists() {
        eprintln!("no deploy/alerts/tests/ — skipping vmalert-tool unittest");
        return;
    }

    let versions = load_versions();
    let vmalert_tool = tag(&versions, "VMALERT_TOOL_TAG");
    let mut any_file = false;
    for entry in std::fs::read_dir(&tests_dir).expect("read_dir tests/") {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with("_test.yaml") {
            continue;
        }
        any_file = true;
        run(
            Command::new("docker").args([
                "run",
                "--rm",
                "-v",
                &format!("{}:/deploy:ro", root.join("deploy").display()),
                &format!("victoriametrics/vmalert-tool:{vmalert_tool}"),
                "unittest",
                &format!("--files=/deploy/alerts/tests/{name}"),
            ]),
            &format!("vmalert-tool unittest {name}"),
        );
    }
    assert!(
        any_file,
        "deploy/alerts/tests/ exists but contains no *_test.yaml files"
    );
}

#[test]
fn amtool_check_config_accepts_alertmanager_yml() {
    check_docker_available();
    let root = repo_root();
    let versions = load_versions();
    let am = tag(&versions, "ALERTMANAGER_TAG");
    run(
        Command::new("docker").args([
            "run",
            "--rm",
            "-v",
            &format!(
                "{}:/etc/alertmanager:ro",
                root.join("deploy/alertmanager").display()
            ),
            "--entrypoint",
            "/bin/amtool",
            &format!("prom/alertmanager:{am}"),
            "check-config",
            "/etc/alertmanager/alertmanager.yml",
        ]),
        "amtool check-config",
    );
}
