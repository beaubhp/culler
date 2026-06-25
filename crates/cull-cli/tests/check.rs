use std::{fs, path::Path, process::Command};

fn write_file(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn cull() -> Command {
    Command::new(env!("CARGO_BIN_EXE_cull"))
}

#[test]
fn default_text_hides_review_findings_and_exits_zero() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/module.py",
        "def public_dead():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull().arg("check").arg(temp.path()).output().unwrap();

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
}

#[test]
fn json_includes_review_findings_without_default_visible_exit_code() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/module.py",
        "def public_dead():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert_eq!(json["summary"]["review"], 1);
    assert_eq!(json["findings"][0]["definition"]["name"], "public_dead");
    assert_eq!(json["findings"][0]["confidence"], "review");
}

#[test]
fn application_text_prints_high_findings_and_exits_one() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/module.py",
        "def public_dead():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--mode")
        .arg("application")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("CULL001 unreferenced-function"));
    assert!(stdout.contains("public_dead"));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
}

#[test]
fn parse_failure_exits_two() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/broken.py", "def broken(:\n    pass\n");
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| {
            diagnostic["severity"] == "error" && diagnostic["path"] == "src/pkg/broken.py"
        }));
}

#[test]
fn invalid_config_mode_exits_two() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'closed_world'\n",
    );

    let output = cull().arg("check").arg(temp.path()).output().unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid [tool.cull].mode"));
}

#[test]
fn invalid_config_mode_in_json_emits_valid_json_error_document() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'closed_world'\n",
    );

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert!(json["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .contains("invalid [tool.cull].mode"));
}

#[test]
fn complete_root_validation_errors_exit_two_in_json() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/app.py", "def main():\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.missing:main']\n",
    );

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| {
            diagnostic["severity"] == "error" && diagnostic["code"] == "CULL_P3001"
        }));
}

#[test]
fn show_review_prints_review_findings_without_failing_ci() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/module.py",
        "def public_dead():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--show-review")
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("CULL001 unreferenced-function"));
    assert!(!stdout.contains("Confidence: high"));
    assert!(stdout.contains("public_dead"));
}

#[test]
fn debug_candidates_includes_suppressed_alternatives() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "def main():\n    pass\n\ndef dormant():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'application'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = cull()
        .arg("debug")
        .arg("candidates")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["schema_version"], 2);
    assert!(json["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .any(|candidate| {
            candidate["definition"]["name"] == "dormant"
                && candidate["rule_id"] == "CULL003"
                && candidate["status"] == "suppressed"
        }));
}

#[test]
fn explain_exact_candidate_id_succeeds() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/module.py",
        "def public_dead():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let candidates = cull()
        .arg("debug")
        .arg("candidates")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(candidates.status.success());
    let json: serde_json::Value = serde_json::from_slice(&candidates.stdout).unwrap();
    let candidate_id = json["candidates"][0]["candidate_id"].as_str().unwrap();

    let output = cull()
        .arg("explain")
        .arg(candidate_id)
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["result"]["status"], "found");
    assert_eq!(json["result"]["candidate"]["candidate_id"], candidate_id);
}

#[test]
fn explain_ambiguous_alias_exits_two_with_candidates() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/a.py", "def dead():\n    pass\n");
    write_file(temp.path(), "src/pkg/b.py", "def dead():\n    pass\n");
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull()
        .arg("explain")
        .arg("dead")
        .arg(temp.path())
        .arg("--mode")
        .arg("application")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["result"]["status"], "ambiguous");
    assert_eq!(json["result"]["candidates"].as_array().unwrap().len(), 2);
}

#[test]
fn allow_partial_keeps_json_valid_and_caps_exit_behavior() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/good.py", "def dead():\n    pass\n");
    write_file(temp.path(), "src/pkg/broken.py", "def broken(:\n    pass\n");
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--mode")
        .arg("application")
        .arg("--allow-partial")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["project_completeness"]["status"], "partial");
    assert_eq!(json["summary"]["high_confidence"], 0);
    assert_eq!(json["findings"][0]["confidence"], "review");
}

#[test]
fn configured_allow_partial_keeps_json_valid_and_caps_exit_behavior() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/good.py", "def dead():\n    pass\n");
    write_file(temp.path(), "src/pkg/broken.py", "def broken(:\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'application'\nallow_partial = true\n",
    );

    let output = cull()
        .arg("check")
        .arg(temp.path())
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["project_completeness"]["status"], "partial");
    assert_eq!(json["summary"]["high_confidence"], 0);
    assert_eq!(json["findings"][0]["confidence"], "review");
    assert!(json["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|diagnostic| {
            diagnostic["code"] == "CULL_P0400" && diagnostic["severity"] == "warning"
        }));
}
