use std::{fs, path::PathBuf, process::Command};

use cull_core::DebugDefinitionsOutput;
use cull_python::{analyze_debug_definitions, DebugDefinitionsOptions};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("part0")
        .join(name)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn analyze_fixture(name: &str) -> DebugDefinitionsOutput {
    analyze_debug_definitions(DebugDefinitionsOptions {
        project_root: fixture(name),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap()
}

fn normalized_json(mut output: DebugDefinitionsOutput) -> String {
    output.project_root = "<PROJECT_ROOT>".to_owned();
    format!("{}\n", serde_json::to_string_pretty(&output).unwrap())
}

#[test]
fn basic_src_layout_snapshot_is_stable() {
    let first = normalized_json(analyze_fixture("basic"));
    let second = normalized_json(analyze_fixture("basic"));

    assert_eq!(first, second);
    assert_eq!(
        first,
        include_str!("snapshots/part0_basic_debug_definitions.json")
    );
}

#[test]
fn flat_layout_snapshot_is_stable() {
    let output = normalized_json(analyze_fixture("flatpkg"));
    assert_eq!(
        output,
        include_str!("snapshots/part0_flatpkg_debug_definitions.json")
    );
}

#[test]
fn invalid_encoding_is_a_structured_diagnostic() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("bad.py"),
        b"# coding: ascii\nvalue = '\xe9'\n",
    )
    .unwrap();

    let output = analyze_debug_definitions(DebugDefinitionsOptions {
        project_root: temp.path().to_path_buf(),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap();

    assert_eq!(output.diagnostics.len(), 1);
    assert_eq!(output.diagnostics[0].code, "CULL_P0101");
    assert!(output.modules.is_empty());
}

#[test]
fn cpython_oracle_agrees_on_basic_top_level_definitions() {
    let source = fixture("basic").join("src/acme/cache.py");
    let oracle = Command::new("python3")
        .arg(workspace_root().join("scripts/cpython_definition_oracle.py"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(oracle.status.success());

    let oracle: serde_json::Value = serde_json::from_slice(&oracle.stdout).unwrap();
    let output = analyze_fixture("basic");
    let module = output
        .modules
        .iter()
        .find(|module| module.name == "acme.cache")
        .unwrap();
    let definitions = module
        .definitions
        .iter()
        .map(|definition| {
            serde_json::json!({
                "kind": match definition.kind {
                    cull_core::DefinitionKind::Function => "function",
                    cull_core::DefinitionKind::Class => "class",
                },
                "name": definition.name,
                "range": definition.range,
                "is_async": definition.is_async,
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(oracle["definitions"], serde_json::Value::Array(definitions));
}

#[test]
fn cpython_oracle_exposes_symtable_facts() {
    let source = fixture("basic").join("src/acme/cache.py");
    let oracle = Command::new("python3")
        .arg(workspace_root().join("scripts/cpython_definition_oracle.py"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(oracle.status.success());

    let oracle: serde_json::Value = serde_json::from_slice(&oracle.stdout).unwrap();
    let child_names = oracle["symtable"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|child| child["name"].as_str().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(child_names, vec!["Cache", "refresh", "helper"]);
}
