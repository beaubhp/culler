use std::{fs, path::Path};

use cull_core::{FindingConfidence, FindingRule, FindingType, ProjectMode, RootCoverage, RootKind};
use cull_python::{analyze_check, CheckOptions};

fn write_file(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn analyze_project(
    root: &Path,
    source_roots: &[&str],
    mode: Option<ProjectMode>,
) -> cull_core::CheckOutput {
    analyze_check(CheckOptions {
        project_root: root.to_path_buf(),
        source_roots: source_roots.iter().map(|path| root.join(path)).collect(),
        target_python: None,
        mode,
        allow_partial: false,
    })
    .unwrap()
}

fn finding<'a>(output: &'a cull_core::CheckOutput, name: &str) -> &'a cull_core::Finding {
    output
        .findings
        .iter()
        .find(|finding| finding.definition.name == name)
        .unwrap_or_else(|| panic!("missing finding for {name}; output: {output:#?}"))
}

fn assert_no_finding(output: &cull_core::CheckOutput, name: &str) {
    assert!(
        output
            .findings
            .iter()
            .all(|finding| finding.definition.name != name),
        "unexpected finding for {name}; output: {output:#?}"
    );
}

fn assert_no_method_findings(output: &cull_core::CheckOutput) {
    assert!(
        output.findings.iter().all(|finding| {
            !finding
                .definition
                .qualified_name
                .split_once("::")
                .is_some_and(|(_, local)| local.contains('.'))
        }),
        "unexpected method finding; output: {output:#?}"
    );
}

fn assert_no_error_diagnostics(output: &cull_core::CheckOutput) {
    assert!(
        output
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != cull_core::DiagnosticSeverity::Error),
        "unexpected diagnostics: {:#?}",
        output.diagnostics
    );
}

#[test]
fn configured_complete_root_finds_dead_weak_cluster() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/cli.py",
        "def main():\n    used()\n\n\
def used():\n    pass\n\n\
def old_entry():\n    old_helper()\n\n\
def old_helper():\n    pass\n\n\
def isolated():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.cli:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Complete);
    assert_no_finding(&output, "used");
    assert_eq!(finding(&output, "old_entry").rule_id, FindingRule::Cull003);
    assert_eq!(finding(&output, "old_helper").rule_id, FindingRule::Cull003);
    assert_eq!(
        finding(&output, "old_entry").finding_type,
        FindingType::RootUnreachable
    );
    assert_eq!(
        finding(&output, "old_entry").confidence,
        FindingConfidence::High
    );
    assert_eq!(finding(&output, "isolated").rule_id, FindingRule::Cull001);
}

#[test]
fn main_guard_root_executes_top_level_branch_without_blanket_liveness() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/cli.py",
        "def main():\n    used_from_main_guard()\n\n\
def used_from_main_guard():\n    pass\n\n\
def dormant():\n    pass\n\n\
if __name__ == '__main__':\n    main()\n",
    );
    write_file(
        temp.path(),
        "src/pkg/importer.py",
        "import pkg.cli\n\n\
def importer_root():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.importer:importer_root']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Complete);
    assert_no_finding(&output, "main");
    assert_no_finding(&output, "used_from_main_guard");
    assert_eq!(finding(&output, "dormant").rule_id, FindingRule::Cull001);
}

#[test]
fn script_roots_are_partial_unless_completeness_is_asserted() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/cli.py", "def main():\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[project.scripts]\ncli = 'pkg.missing:main'\n\n[tool.cull]\nsrc = 'src'\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Partial);
    assert!(output.roots.iter().any(|root| !root.resolved));
}

#[test]
fn dynamic_script_metadata_uses_static_table_when_available() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/cli.py", "def main():\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[project]\ndynamic = ['scripts']\n\n\
[project.scripts]\ncli = 'pkg.cli:main'\n\n\
[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Complete);
    assert!(output.roots.iter().any(|root| root.resolved));
}

#[test]
fn dynamic_script_metadata_without_static_table_fails_complete_coverage() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/cli.py", "def main():\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[project]\ndynamic = ['scripts']\n\n\
[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.cli:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert!(output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == cull_core::DiagnosticSeverity::Error));
}

#[test]
fn configured_object_root_imports_module_before_calling_target() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "def boot():\n    boot_helper()\n\n\
boot()\n\n\
def boot_helper():\n    pass\n\n\
def main():\n    pass\n\n\
def dormant():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_no_finding(&output, "boot");
    assert_no_finding(&output, "boot_helper");
    assert_eq!(finding(&output, "dormant").rule_id, FindingRule::Cull001);
}

#[test]
fn absent_root_coverage_disables_root_unreachable_findings() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/legacy.py",
        "def old_entry():\n    old_helper()\n\n\
def old_helper():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Absent);
    assert_eq!(finding(&output, "old_entry").rule_id, FindingRule::Cull001);
    assert!(output
        .findings
        .iter()
        .all(|finding| finding.rule_id != FindingRule::Cull003));
}

#[test]
fn complete_coverage_fails_when_script_target_is_unresolved() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/cli.py", "def main():\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[project.scripts]\ncli = 'pkg.missing:main'\n\n\
[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert!(output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == cull_core::DiagnosticSeverity::Error));
}

#[test]
fn malformed_script_reference_is_configuration_error() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[project.scripts]\ncli = 'pkg.cli'\n\n[tool.cull]\nsrc = 'src'\n",
    );

    let error = analyze_check(CheckOptions {
        project_root: temp.path().to_path_buf(),
        source_roots: vec![temp.path().join("src")],
        target_python: None,
        mode: Some(ProjectMode::Application),
        allow_partial: false,
    })
    .unwrap_err();

    assert!(error
        .message
        .contains("invalid project script object reference"));
}

#[test]
fn test_reachability_does_not_become_production_reachability() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/cli.py", "def main():\n    pass\n");
    write_file(temp.path(), "src/pkg/lib.py", "def _helper():\n    pass\n");
    write_file(
        temp.path(),
        "tests/test_lib.py",
        "from pkg.lib import _helper\n\n\
def test_helper():\n    _helper()\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = ['src', 'tests']\nroot_coverage = 'complete'\nroots = ['pkg.cli:main']\n",
    );

    let output = analyze_project(
        temp.path(),
        &["src", "tests"],
        Some(ProjectMode::Application),
    );

    assert_no_error_diagnostics(&output);
    let helper = finding(&output, "_helper");
    assert_eq!(helper.rule_id, FindingRule::Cull003);
    assert!(!helper.reachability.production_reachable);
    assert!(helper.reachability.test_reachable);
}

#[test]
fn auto_partial_root_unreachable_findings_are_review() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "def main():\n    pass\n\n\
def _old_entry():\n    _old_helper()\n\n\
def _old_helper():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Auto));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Partial);
    let old_entry = finding(&output, "_old_entry");
    assert_eq!(old_entry.rule_id, FindingRule::Cull003);
    assert_eq!(old_entry.confidence, FindingConfidence::Review);
}

#[test]
fn auto_complete_private_root_unreachable_findings_can_be_high() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "def main():\n    pass\n\n\
def _old_entry():\n    _old_helper()\n\n\
def _old_helper():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Auto));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::Complete);
    let old_entry = finding(&output, "_old_entry");
    assert_eq!(old_entry.rule_id, FindingRule::Cull003);
    assert_eq!(old_entry.confidence, FindingConfidence::High);
}

#[test]
fn auto_external_surface_protects_exported_private_helpers() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "src/pkg/__init__.py",
        "from .api import public_api\n__all__ = ['public_api']\n",
    );
    write_file(
        temp.path(),
        "src/pkg/api.py",
        "def public_api():\n    _helper()\n\n\
def _helper():\n    pass\n\n\
def _dead():\n    pass\n",
    );
    write_file(temp.path(), "pyproject.toml", "[tool.cull]\nsrc = 'src'\n");

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Auto));

    assert_no_error_diagnostics(&output);
    assert_no_finding(&output, "public_api");
    assert_no_finding(&output, "_helper");
    assert_eq!(finding(&output, "_dead").rule_id, FindingRule::Cull001);
}

#[test]
fn lazy_annotation_reference_does_not_establish_runtime_reachability() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "def main(value: lazy_only) -> None:\n    pass\n\n\
def lazy_only():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    let lazy_only = finding(&output, "lazy_only");
    assert_eq!(lazy_only.rule_id, FindingRule::Cull003);
    assert!(!lazy_only.reachability.production_reachable);
    assert!(lazy_only
        .reference_phases
        .iter()
        .any(|phase| phase.phase == cull_core::ReferencePhase::LazyAnnotation));
}

#[test]
fn library_external_surface_protects_private_helpers() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "src/pkg/__init__.py",
        "from .api import api\n__all__ = ['api']\n",
    );
    write_file(
        temp.path(),
        "src/pkg/api.py",
        "def api():\n    _helper()\n\n\
def _helper():\n    pass\n\n\
def _dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'library'\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Library));

    assert_no_error_diagnostics(&output);
    assert_eq!(output.root_coverage, RootCoverage::NotApplicable);
    assert_no_finding(&output, "_helper");
    assert_eq!(finding(&output, "_dead").rule_id, FindingRule::Cull001);
}

#[test]
fn library_private_dead_cluster_can_be_high_confidence_root_unreachable() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "src/pkg/__init__.py",
        "from .api import public_api\n__all__ = ['public_api']\n",
    );
    write_file(
        temp.path(),
        "src/pkg/api.py",
        "def public_api():\n    _live_helper()\n\n\
def _live_helper():\n    pass\n\n\
def _old_entry():\n    _old_helper()\n\n\
def _old_helper():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'library'\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Library));

    assert_no_error_diagnostics(&output);
    assert_no_finding(&output, "_live_helper");
    let old_entry = finding(&output, "_old_entry");
    assert_eq!(old_entry.rule_id, FindingRule::Cull003);
    assert_eq!(old_entry.confidence, FindingConfidence::High);
}

#[test]
fn direct_class_construction_activates_init_but_not_every_method() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        concat!(
            "def main():\n",
            "    user = User()\n\n",
            "class User:\n",
            "    def __init__(self):\n",
            "        init_helper()\n\n",
            "    def save(self):\n",
            "        save_helper()\n\n",
            "def init_helper():\n",
            "    pass\n\n",
            "def save_helper():\n",
            "    pass\n",
        ),
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_no_method_findings(&output);
    assert_no_finding(&output, "init_helper");
    assert_eq!(
        finding(&output, "save_helper").rule_id,
        FindingRule::Cull003
    );
}

#[test]
fn direct_class_construction_activates_static_metaclass_call() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        concat!(
            "def main():\n",
            "    User()\n\n",
            "class Meta(type):\n",
            "    def __call__(cls):\n",
            "        metaclass_helper()\n\n",
            "class User(metaclass=Meta):\n",
            "    def __init__(self):\n",
            "        init_helper()\n\n",
            "    def save(self):\n",
            "        save_helper()\n\n",
            "def metaclass_helper():\n",
            "    pass\n\n",
            "def init_helper():\n",
            "    pass\n\n",
            "def save_helper():\n",
            "    pass\n",
        ),
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_no_method_findings(&output);
    assert_no_finding(&output, "metaclass_helper");
    assert_no_finding(&output, "init_helper");
    assert_eq!(
        finding(&output, "save_helper").rule_id,
        FindingRule::Cull003
    );
}

#[test]
fn unresolved_metaclass_construction_keeps_method_side_findings_review() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        concat!(
            "def main():\n",
            "    User()\n\n",
            "def dynamic_meta():\n",
            "    return type\n\n",
            "class User(metaclass=dynamic_meta()):\n",
            "    def __init__(self):\n",
            "        pass\n\n",
            "    def save(self):\n",
            "        save_helper()\n\n",
            "def save_helper():\n",
            "    pass\n",
        ),
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_no_method_findings(&output);
    let helper = finding(&output, "save_helper");
    assert_eq!(helper.rule_id, FindingRule::Cull003);
    assert_eq!(
        helper.confidence,
        FindingConfidence::Review,
        "output: {output:#?}"
    );
    assert!(helper.uncertainty.iter().any(|uncertainty| uncertainty.kind
        == cull_core::FindingUncertaintyKind::DynamicClassConstruction));
}

#[test]
fn configured_nested_method_root_resolves_exactly() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "class Service:\n    def run(self):\n        helper()\n\n\
def helper():\n    pass\n\n\
def _dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:Service.run']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_no_method_findings(&output);
    assert_no_finding(&output, "helper");
    assert_eq!(finding(&output, "_dead").rule_id, FindingRule::Cull001);
}

#[test]
fn unresolved_configured_module_root_preserves_root_kind() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/app.py", "def main():\n    pass\n");
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.missing']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert!(output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == cull_core::DiagnosticSeverity::Error));
    let root = output
        .roots
        .iter()
        .find(|root| root.target == "pkg.missing")
        .expect("missing configured module root");
    assert_eq!(root.kind, RootKind::ConfiguredModule);
    assert!(!root.resolved);
}

#[test]
fn callable_argument_escape_conservatively_activates_body() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        "def main():\n    register(callback)\n\n\
def register(func):\n    pass\n\n\
def callback():\n    helper()\n\n\
def helper():\n    pass\n",
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = analyze_project(temp.path(), &["src"], Some(ProjectMode::Application));

    assert_no_error_diagnostics(&output);
    assert_no_finding(&output, "callback");
    assert_no_finding(&output, "helper");
}
