//! `tracon-plugin.toml` against the contract and this build.

const MANIFEST: &str = include_str!("../tracon-plugin.toml");

#[test]
fn the_manifest_matches_the_crate_and_the_contract() {
    let manifest: toml::Table = toml::from_str(MANIFEST).unwrap();
    let mut keys: Vec<&str> = manifest.keys().map(String::as_str).collect();
    keys.sort();
    // A key written after `[ui]` would land inside it, and the node refuses
    // unknown keys, so the set is exact. No [[tool]]: tools/list is empty.
    assert_eq!(keys, ["contract", "exec", "name", "ui", "version"]);
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
    let ui = manifest["ui"].as_table().unwrap();
    let mut ui_keys: Vec<&str> = ui.keys().map(String::as_str).collect();
    ui_keys.sort();
    assert_eq!(ui_keys, ["entry", "title"]);
    assert_eq!(ui["entry"].as_str(), Some("ui/index.html"));
    assert_eq!(ui["title"].as_str(), Some("hoja"));
}
