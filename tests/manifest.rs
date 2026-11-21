//! `tracon-plugin.toml` against the contract and this build.

const MANIFEST: &str = include_str!("../tracon-plugin.toml");

#[test]
fn the_manifest_matches_the_crate_and_the_contract() {
    let manifest: toml::Table = toml::from_str(MANIFEST).unwrap();
    let mut keys: Vec<&str> = manifest.keys().map(String::as_str).collect();
    keys.sort();
    // A key written after `[ui]` or a `[[tool]]` would land inside it, and
    // the node refuses unknown keys, so the set is exact.
    assert_eq!(
        keys,
        [
            "contract",
            "exec",
            "name",
            "node_methods",
            "tool",
            "ui",
            "version"
        ]
    );
    assert_eq!(manifest["name"].as_str(), Some(hoja::contract::PLUGIN));
    assert_eq!(
        manifest["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(manifest["contract"].as_integer(), Some(1));
    assert_eq!(
        manifest["exec"],
        toml::Value::Array(vec!["bin/hoja".into(), "serve".into()])
    );
    // The methods hoja calls the node back with, and no others: the node
    // answers only these, and an update's card shows any change to them.
    let methods: Vec<&str> = manifest["node_methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap())
        .collect();
    assert_eq!(methods, hoja::node_cards::NODE_METHODS);
    let ui = manifest["ui"].as_table().unwrap();
    let mut ui_keys: Vec<&str> = ui.keys().map(String::as_str).collect();
    ui_keys.sort();
    assert_eq!(ui_keys, ["entry", "title"]);
    assert_eq!(ui["entry"].as_str(), Some("ui/index.html"));
    assert_eq!(ui["title"].as_str(), Some("hoja"));
}

/// The node stops a start whose `tools/list` disagrees with the manifest, so
/// the manifest's `[[tool]]`s are exactly the tools, in order, with their
/// hints, and none is consequential: hoja's gates hold an agent's write as a
/// card where the person must decide it.
#[test]
fn the_manifest_tools_are_the_ones_serve_lists() {
    let manifest: toml::Table = toml::from_str(MANIFEST).unwrap();
    let declared: Vec<(String, String)> = manifest["tool"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            let t = t.as_table().unwrap();
            let mut keys: Vec<&str> = t.keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, ["hint", "name"], "{t:?}");
            (
                t["name"].as_str().unwrap().to_string(),
                t["hint"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let listed: Vec<(String, String)> = hoja::ops::tools()
        .iter()
        .map(|t| {
            let read = t["annotations"]["readOnlyHint"] == true;
            (
                t["name"].as_str().unwrap().to_string(),
                if read { "read" } else { "write" }.to_string(),
            )
        })
        .collect();
    assert_eq!(declared, listed);
}
