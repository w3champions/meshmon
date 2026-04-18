//! Hermetic contract test: panels.json ⊆ dashboards/*.json.
//!
//! Ported from `grafana/verify-panels.mjs` (pure Node, now deleted).
//! Runs under `cargo test --workspace` with no docker dependency.

use serde_json::Value;
use std::{fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn load_json(path: &std::path::Path) -> Value {
    let raw = fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {path:?}: {e}"))
}

fn collect_panel_ids(dash: &Value, out: &mut Vec<u64>) {
    if let Some(arr) = dash.get("panels").and_then(Value::as_array) {
        for p in arr {
            if let Some(id) = p.get("id").and_then(Value::as_u64) {
                out.push(id);
            }
            collect_panel_ids(p, out);
        }
    }
}

#[test]
fn panels_contract_holds() {
    let root = repo_root();
    let panels_path = root.join("grafana/panels.json");
    let panels: Value = load_json(&panels_path);

    let obj = panels
        .as_object()
        .expect("panels.json must be a top-level object");

    for (uid_key, entry) in obj {
        let dash_path = root
            .join("grafana/dashboards")
            .join(format!("{uid_key}.json"));
        assert!(
            dash_path.exists(),
            "panels.json entry `{uid_key}` has no grafana/dashboards/{uid_key}.json"
        );
        let dash = load_json(&dash_path);

        let dash_uid = dash
            .get("uid")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{dash_path:?}: missing top-level uid"));
        assert_eq!(
            dash_uid, uid_key,
            "{dash_path:?}: top-level uid `{dash_uid}` does not match panels.json key `{uid_key}`"
        );

        let mut actual_ids = Vec::new();
        collect_panel_ids(&dash, &mut actual_ids);
        if let Some(expected) = entry.get("panels").and_then(Value::as_object) {
            for (name, id_v) in expected {
                let expected_id = id_v
                    .as_u64()
                    .unwrap_or_else(|| panic!("panels.json: {uid_key}.panels.{name} is not a u64"));
                assert!(
                    actual_ids.contains(&expected_id),
                    "panels.json: {uid_key}.panels.{name} = {expected_id} but no panel with that id in {dash_path:?}"
                );
            }
        }

        if let Some(expected_vars) = entry.get("variables").and_then(Value::as_array) {
            let actual_vars: Vec<&str> = dash
                .pointer("/templating/list")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.get("name").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            for var in expected_vars {
                let name = var.as_str().unwrap_or_else(|| {
                    panic!("panels.json: {uid_key}.variables entries must be strings")
                });
                assert!(
                    actual_vars.contains(&name),
                    "panels.json: {uid_key}.variables references `{name}` but {dash_path:?} has no such templating variable"
                );
            }
        }
    }
}

#[test]
fn dashboards_parse_as_json() {
    let root = repo_root();
    let dir = root.join("grafana/dashboards");
    let entries = fs::read_dir(&dir).expect("read grafana/dashboards");
    let mut count = 0;
    for entry in entries {
        let entry = entry.unwrap();
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        load_json(&entry.path());
        count += 1;
    }
    assert!(
        count > 0,
        "no dashboard JSONs found under grafana/dashboards"
    );
}
