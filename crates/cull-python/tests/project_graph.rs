use std::{fs, path::Path};

use cull_core::{FindingConfidence, ProjectMode};
use cull_python::{analyze_check, CheckOptions};

fn write_file(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn analyze_project(
    root: &Path,
    source_roots: &[&str],
    mode: ProjectMode,
) -> cull_core::CheckOutput {
    analyze_check(CheckOptions {
        project_root: root.to_path_buf(),
        source_roots: source_roots.iter().map(|path| root.join(path)).collect(),
        target_python: None,
        mode: Some(mode),
        allow_partial: false,
    })
    .unwrap()
}

fn finding<'a>(output: &'a cull_core::CheckOutput, name: &str) -> Option<&'a cull_core::Finding> {
    output
        .findings
        .iter()
        .find(|finding| finding.definition.name == name)
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
fn path_entry_first_provider_precedence_shadows_later_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "first/pkg/mod.py",
        "def _winner_dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "second/pkg/mod.py",
        "def _shadowed_dead():\n    pass\n",
    );

    let output = analyze_project(temp.path(), &["first", "second"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "CULL_P0005"));
    assert!(finding(&output, "_winner_dead").is_some());
    assert!(finding(&output, "_shadowed_dead").is_none());
}

#[test]
fn local_namespace_package_portions_resolve_across_source_roots() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "first/ns/a.py",
        "def Used():\n    pass\n\ndef _dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "second/ns/b.py",
        "from ns.a import Used\n\nUsed()\n",
    );

    let output = analyze_project(temp.path(), &["first", "second"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(finding(&output, "Used").is_none());
    assert_eq!(
        finding(&output, "_dead").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn module_attribute_imports_prevent_false_positive_findings() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/a.py",
        "def used():\n    pass\n\ndef _dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/consumer.py",
        "import pkg.a\n\npkg.a.used()\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(finding(&output, "used").is_none());
    assert_eq!(
        finding(&output, "_dead").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn circular_local_imports_attach_partial_initialization_uncertainty() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/a.py",
        "from . import b\n\n\ndef _cycle_dead():\n    pass\n",
    );
    write_file(temp.path(), "src/pkg/b.py", "from . import a\n");

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);
    let finding = finding(&output, "_cycle_dead").unwrap();

    assert_no_error_diagnostics(&output);
    assert_eq!(finding.confidence, FindingConfidence::Review);
    assert!(finding
        .uncertainty
        .iter()
        .any(|uncertainty| uncertainty.kind
            == cull_core::FindingUncertaintyKind::PartialInitialization));
}

#[test]
fn literal_dynamic_import_does_not_reference_every_definition_in_module() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/a.py",
        "def _dead_after_module_load():\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/consumer.py",
        "import importlib\n\nimportlib.import_module('pkg.a')\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert_eq!(
        finding(&output, "_dead_after_module_load")
            .unwrap()
            .confidence,
        FindingConfidence::High
    );
}

#[test]
fn literal_dynamic_import_alias_uses_binding_provenance() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/a.py",
        "def used():\n    pass\n\n\ndef _dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/consumer.py",
        "import importlib as il\n\nmodule = il.import_module('pkg.a')\nmodule.used()\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(finding(&output, "used").is_none());
    assert_eq!(
        finding(&output, "_dead").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn shadowed_import_module_is_not_treated_as_stdlib_dynamic_import() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/a.py", "def used():\n    pass\n");
    write_file(
        temp.path(),
        "src/pkg/consumer.py",
        "def import_module(name):\n    return None\n\nmodule = import_module('pkg.a')\nmodule.used()\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert_eq!(
        finding(&output, "used").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn importlib_identity_does_not_propagate_through_arbitrary_attributes() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/a.py", "def used():\n    pass\n");
    write_file(
        temp.path(),
        "src/pkg/consumer.py",
        "import importlib\n\nmodule = importlib.fake.import_module('pkg.a')\nmodule.used()\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert_eq!(
        finding(&output, "used").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn finding_order_is_stable_across_file_creation_order() {
    fn build_project(root: &Path, reverse: bool) {
        let files = [
            ("src/pkg/__init__.py", ""),
            ("src/pkg/a.py", "def _a_dead():\n    pass\n"),
            ("src/pkg/b.py", "def _b_dead():\n    pass\n"),
            ("src/pkg/c.py", "def _c_dead():\n    pass\n"),
        ];
        let iter: Box<dyn Iterator<Item = _>> = if reverse {
            Box::new(files.into_iter().rev())
        } else {
            Box::new(files.into_iter())
        };
        for (path, contents) in iter {
            write_file(root, path, contents);
        }
    }

    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    build_project(first.path(), false);
    build_project(second.path(), true);

    let first_output = analyze_project(first.path(), &["src"], ProjectMode::Application);
    let second_output = analyze_project(second.path(), &["src"], ProjectMode::Application);
    let first_findings = first_output
        .findings
        .iter()
        .map(|finding| {
            (
                finding.definition.file.clone(),
                finding.definition.name.clone(),
                finding.confidence,
            )
        })
        .collect::<Vec<_>>();
    let second_findings = second_output
        .findings
        .iter()
        .map(|finding| {
            (
                finding.definition.file.clone(),
                finding.definition.name.clone(),
                finding.confidence,
            )
        })
        .collect::<Vec<_>>();

    assert_eq!(first_findings, second_findings);
}

#[test]
fn explicit_all_and_reexport_chains_are_definite_export_references() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "src/pkg/models.py",
        "class User:\n    pass\n\nclass _Internal:\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/api.py",
        "from .models import User as PublicUser\n",
    );
    write_file(
        temp.path(),
        "src/pkg/__init__.py",
        "from .api import PublicUser\n__all__ = ['PublicUser']\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(finding(&output, "User").is_none());
    assert!(finding(&output, "PublicUser").is_none());
    assert!(finding(&output, "_Internal").is_some());
}

#[test]
fn package_reexports_are_definite_exports_independent_of_mode() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "src/pkg/models.py",
        "class User:\n    pass\n\nclass Team:\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/__init__.py",
        "from .models import Team as PublicTeam\nfrom .models import User\n\n\
def public_dead():\n    pass\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(finding(&output, "User").is_none());
    assert!(finding(&output, "Team").is_none());
    assert_eq!(
        finding(&output, "public_dead").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn package_public_surface_is_conservative_outside_application_mode() {
    let temp = tempfile::tempdir().unwrap();
    write_file(
        temp.path(),
        "src/pkg/__init__.py",
        "def public_api():\n    pass\n\n\ndef _private_dead():\n    pass\n",
    );

    let auto = analyze_project(temp.path(), &["src"], ProjectMode::Auto);
    let application = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&auto);
    assert_no_error_diagnostics(&application);
    assert!(finding(&auto, "public_api").is_none());
    assert_eq!(
        finding(&auto, "_private_dead").unwrap().confidence,
        FindingConfidence::High
    );
    assert_eq!(
        finding(&application, "public_api").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn public_but_not_exported_definitions_are_review_in_auto_and_high_in_application() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/module.py",
        "def public_dead():\n    pass\n",
    );

    let auto = analyze_project(temp.path(), &["src"], ProjectMode::Auto);
    let application = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&auto);
    assert_no_error_diagnostics(&application);
    assert_eq!(
        finding(&auto, "public_dead").unwrap().confidence,
        FindingConfidence::Review
    );
    assert_eq!(
        finding(&application, "public_dead").unwrap().confidence,
        FindingConfidence::High
    );
}

#[test]
fn conditional_all_star_import_uses_explicit_and_absent_path_public_surface() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/source.py",
        "if FLAG:\n    __all__ = ['_Hidden']\n\n\
def _Hidden():\n    pass\n\n\
def Public():\n    pass\n\n\
def _dead():\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/consumer.py",
        "from .source import *\n\n_Hidden()\nPublic()\n",
    );

    let output = analyze_project(temp.path(), &["src"], ProjectMode::Application);

    assert_no_error_diagnostics(&output);
    assert!(finding(&output, "_Hidden").is_none());
    assert!(finding(&output, "Public").is_none());
    assert!(finding(&output, "_dead").is_some());
}
