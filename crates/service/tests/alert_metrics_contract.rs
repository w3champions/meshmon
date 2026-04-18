//! Hermetic contract test: every meshmon_* metric referenced in
//! deploy/alerts/rules.yaml must appear as a quoted string literal in
//! crates/service/src/.
//!
//! Ported from `scripts/check-rule-metrics.sh` (now deleted).

use std::{collections::BTreeSet, fs, path::PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn collect_rs(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.unwrap();
        let p = entry.path();
        if p.is_dir() {
            collect_rs(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}

fn extract_meshmon_metrics(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let prefix = b"meshmon_";
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i + prefix.len() <= bytes.len() {
        if &bytes[i..i + prefix.len()] == prefix {
            let start = i;
            let mut end = start + prefix.len();
            while end < bytes.len() {
                let b = bytes[end];
                if b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' {
                    end += 1;
                } else {
                    break;
                }
            }
            if end > start + prefix.len() {
                out.insert(std::str::from_utf8(&bytes[start..end]).unwrap().to_string());
            }
            i = end.max(start + 1);
        } else {
            i += 1;
        }
    }
    out
}

#[test]
fn rules_metrics_are_emitted_by_service() {
    let root = repo_root();
    let rules = fs::read_to_string(root.join("deploy/alerts/rules.yaml"))
        .expect("read deploy/alerts/rules.yaml");
    let metrics = extract_meshmon_metrics(&rules);

    if metrics.is_empty() {
        eprintln!("no meshmon_* metrics in rules.yaml — skipping cross-check");
        return;
    }

    let mut rs_files = Vec::new();
    collect_rs(&root.join("crates/service/src"), &mut rs_files);
    let haystacks: Vec<String> = rs_files
        .iter()
        .map(|p| fs::read_to_string(p).unwrap())
        .collect();

    let mut missing = Vec::new();
    for m in &metrics {
        let quoted = format!("\"{m}\"");
        let found = haystacks.iter().any(|h| h.contains(&quoted));
        if !found {
            missing.push(m.clone());
        }
    }

    assert!(
        missing.is_empty(),
        "rules.yaml references {} metric(s) absent from crates/service/src:\n  - {}",
        missing.len(),
        missing.join("\n  - ")
    );
}
