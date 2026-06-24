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
    assert_eq!(json["schema_version"], 1);
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
