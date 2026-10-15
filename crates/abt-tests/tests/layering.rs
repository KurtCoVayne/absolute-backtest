//! The layering of the workspace (docs/spec/00-architecture.md, A-D2 and
//! Definition 2.1): a layer depends on the shared crates `abt-wire` and
//! `abt-agg` and on no other layer, as a normal, build or dev dependency; the
//! shared crates depend on no layer; only the front and the cross-layer
//! suites may name several layers.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;

fn workspace_dependencies() -> BTreeMap<String, BTreeSet<String>> {
    let out = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(abt_tests::repo_root())
        .output()
        .expect("cargo metadata runs");
    assert!(out.status.success(), "cargo metadata: {}", String::from_utf8_lossy(&out.stderr));
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("cargo metadata prints JSON");
    let mut deps = BTreeMap::new();
    for p in meta["packages"].as_array().expect("packages") {
        let name = p["name"].as_str().unwrap().to_string();
        let names: BTreeSet<String> = p["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap().to_string())
            .filter(|d| d.starts_with("abt"))
            .collect();
        deps.insert(name, names);
    }
    deps
}

#[test]
fn no_layer_depends_on_another() {
    let deps = workspace_dependencies();
    let shared: BTreeSet<&str> = ["abt-wire", "abt-agg"].into();
    for layer in ["abt-data", "abt-kernel", "abt-query"] {
        let d = deps.get(layer).unwrap_or_else(|| panic!("{} is a workspace member", layer));
        let foreign: Vec<&String> = d.iter().filter(|n| !shared.contains(n.as_str())).collect();
        assert!(foreign.is_empty(), "{} depends on {:?}: a layer reaches another only through the protocol (A-D3)", layer, foreign);
    }
    for crate_ in shared.iter().copied() {
        let d = deps.get(crate_).unwrap_or_else(|| panic!("{} is a workspace member", crate_));
        assert!(d.iter().all(|n| shared.contains(n.as_str())), "{} depends on {:?}: a shared crate depends on no layer", crate_, d);
    }
}
