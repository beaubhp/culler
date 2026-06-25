use std::{collections::BTreeSet, fs, path::Path};

use cull_core::{
    CandidateStatus, DiagnosticSeverity, FindingConfidence, FindingRule, FindingUncertaintyKind,
    ProjectCompletenessStatus, ProjectMode, RootCoverage, SecondaryCondition,
    SuppressionReasonKind, UncertaintyEffect, UncertaintyRegionKind,
};
use cull_python::{analyze_check, analyze_debug_candidates, CheckOptions, DebugCandidatesOptions};

fn write_file(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn check_project(root: &Path, mode: ProjectMode, allow_partial: bool) -> cull_core::CheckOutput {
    analyze_check(CheckOptions {
        project_root: root.to_path_buf(),
        source_roots: vec![root.join("src")],
        target_python: None,
        mode: Some(mode),
        allow_partial,
    })
    .unwrap()
}

fn debug_candidates(
    root: &Path,
    mode: ProjectMode,
    allow_partial: bool,
) -> cull_core::DebugCandidatesOutput {
    analyze_debug_candidates(DebugCandidatesOptions {
        project_root: root.to_path_buf(),
        source_roots: vec![root.join("src")],
        target_python: None,
        mode: Some(mode),
        allow_partial,
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

#[test]
fn schema_v2_findings_have_ids_evidence_and_reported_status() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/mod.py", "def dead():\n    pass\n");

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let dead = finding(&output, "dead");

    assert_eq!(output.schema_version, 2);
    assert_eq!(output.analysis.mode, ProjectMode::Application);
    assert_eq!(dead.status, CandidateStatus::Reported);
    assert!(dead.finding_id.starts_with("CULL001-"));
    assert_eq!(dead.id, dead.finding_id);
    assert!(!dead.evidence.is_empty());
    assert!(dead
        .evidence
        .iter()
        .any(|evidence| evidence.kind == cull_core::EvidenceKind::NoInboundReferences));
}

#[test]
fn debug_candidates_include_suppressed_non_primary_rule_alternatives() {
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

    let output = check_project(temp.path(), ProjectMode::Application, false);
    assert_eq!(output.root_coverage, RootCoverage::Complete);
    let dormant = finding(&output, "dormant");
    assert_eq!(dormant.rule_id, FindingRule::Cull001);
    assert_eq!(
        dormant.secondary_conditions,
        vec![SecondaryCondition::AlsoRootUnreachable]
    );

    let debug = debug_candidates(temp.path(), ProjectMode::Application, false);
    assert!(debug.candidates.iter().any(|candidate| {
        candidate.definition.name == "dormant"
            && candidate.rule_id == FindingRule::Cull003
            && candidate.status == CandidateStatus::Suppressed
            && candidate
                .suppression_reasons
                .iter()
                .any(|reason| reason.kind == SuppressionReasonKind::NonPrimaryRuleAlternative)
    }));
}

#[test]
fn output_contract_invariants_hold_for_reported_and_suppressed_candidates() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/app.py",
        concat!(
            "def main():\n",
            "    dormant()\n\n",
            "def dormant():\n",
            "    pass\n\n",
            "def public_review():\n",
            "    pass\n\n",
            "name = 'public_review'\n",
            "globals()[name]\n",
        ),
    );
    write_file(
        temp.path(),
        "pyproject.toml",
        "[tool.cull]\nsrc = 'src'\nmode = 'application'\nroot_coverage = 'complete'\nroots = ['pkg.app:main']\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let mut finding_ids = BTreeSet::new();
    for finding in &output.findings {
        assert!(finding_ids.insert(finding.finding_id.clone()));
        assert!(!finding.evidence.is_empty());
        if finding.confidence == FindingConfidence::Review {
            assert!(!finding.blockers.is_empty());
        }
    }

    let debug = debug_candidates(temp.path(), ProjectMode::Application, false);
    let mut candidate_ids = BTreeSet::new();
    let mut suppressed = 0;
    for candidate in &debug.candidates {
        assert!(candidate_ids.insert(candidate.candidate_id.clone()));
        if candidate.status == CandidateStatus::Suppressed {
            suppressed += 1;
            assert!(!candidate.suppression_reasons.is_empty());
        }
    }
    assert!(suppressed > 0);
}

#[test]
fn partial_analysis_caps_high_confidence_and_keeps_json_complete() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/good.py", "def dead():\n    pass\n");
    write_file(temp.path(), "src/pkg/broken.py", "def broken(:\n    pass\n");

    let output = check_project(temp.path(), ProjectMode::Application, true);
    let dead = finding(&output, "dead");

    assert_eq!(
        output.project_completeness.status,
        ProjectCompletenessStatus::Partial
    );
    assert_eq!(
        output.project_completeness.confidence_ceiling,
        Some(FindingConfidence::Review)
    );
    assert_eq!(dead.confidence, FindingConfidence::Review);
    assert_eq!(output.summary.high_confidence, 0);
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "CULL_P0400" && diagnostic.severity == DiagnosticSeverity::Warning
    }));
    assert!(output.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "CULL_P0201" && diagnostic.severity == DiagnosticSeverity::Warning
    }));
}

#[test]
fn literal_getattr_on_known_module_is_an_exact_reference() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/api.py", "def handler():\n    pass\n");
    write_file(
        temp.path(),
        "src/pkg/use.py",
        "import pkg.api as api\n\ngetattr(api, 'handler')\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    assert!(output
        .findings
        .iter()
        .all(|finding| finding.definition.name != "handler"));
}

#[test]
fn literal_hasattr_on_known_module_is_an_exact_reference() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/api.py", "def handler():\n    pass\n");
    write_file(
        temp.path(),
        "src/pkg/use.py",
        "import pkg.api as api\n\nhasattr(api, 'handler')\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    assert!(output
        .findings
        .iter()
        .all(|finding| finding.definition.name != "handler"));
}

#[test]
fn dynamic_getattr_uncertainty_is_localized_to_receiver_module() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/api.py", "def handler():\n    pass\n");
    write_file(
        temp.path(),
        "src/pkg/other.py",
        "def unrelated():\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/use.py",
        "import pkg.api as api\n\nname = 'handler'\ngetattr(api, name)\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let handler = finding(&output, "handler");
    let unrelated = finding(&output, "unrelated");

    assert_eq!(handler.confidence, FindingConfidence::Review);
    assert!(handler.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::DynamicAttributeRead
            && uncertainty.affected_region.kind == UncertaintyRegionKind::ModuleNamespace
            && uncertainty.affected_region.target == "pkg.api"
            && uncertainty
                .effects
                .contains(&UncertaintyEffect::MayReadAnyAttribute)
    }));
    assert_eq!(unrelated.confidence, FindingConfidence::High);
}

#[test]
fn eval_and_exec_uncertainty_is_local_to_current_module() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/mod.py",
        "def dead():\n    pass\n\nexec('dead()')\neval('dead')\n",
    );
    write_file(
        temp.path(),
        "src/pkg/other.py",
        "def unrelated():\n    pass\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let dead = finding(&output, "dead");
    let unrelated = finding(&output, "unrelated");

    assert_eq!(dead.confidence, FindingConfidence::Review);
    assert!(dead.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::DynamicExecution
            && uncertainty.affected_region.kind == UncertaintyRegionKind::ModuleNamespace
            && uncertainty.affected_region.target == "pkg.mod"
            && uncertainty
                .effects
                .contains(&UncertaintyEffect::MayMutateNamespace)
    }));
    assert_eq!(unrelated.confidence, FindingConfidence::High);
}

#[test]
fn namespace_mapping_index_and_mutation_are_local_review_blockers() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/mod.py",
        "def dead():\n    pass\n\nname = 'dead'\nglobals()[name]\nglobals()['new_name'] = object()\n",
    );
    write_file(
        temp.path(),
        "src/pkg/other.py",
        "def unrelated():\n    pass\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let dead = finding(&output, "dead");
    let unrelated = finding(&output, "unrelated");

    assert_eq!(dead.confidence, FindingConfidence::Review);
    assert!(dead.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::DynamicAttributeRead
            && uncertainty.affected_region.target == "pkg.mod"
    }));
    assert!(dead.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::NamespaceMutation
            && uncertainty.affected_region.target == "pkg.mod"
    }));
    assert_eq!(unrelated.confidence, FindingConfidence::High);
}

#[test]
fn setattr_and_delattr_mutation_uncertainty_is_local_to_receiver_module() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(temp.path(), "src/pkg/api.py", "def handler():\n    pass\n");
    write_file(
        temp.path(),
        "src/pkg/other.py",
        "def unrelated():\n    pass\n",
    );
    write_file(
        temp.path(),
        "src/pkg/use.py",
        "import pkg.api as api\n\nsetattr(api, 'handler', object())\ndelattr(api, 'handler')\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let handler = finding(&output, "handler");
    let unrelated = finding(&output, "unrelated");

    assert_eq!(handler.confidence, FindingConfidence::Review);
    assert!(handler.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::NamespaceMutation
            && uncertainty.affected_region.kind == UncertaintyRegionKind::ModuleNamespace
            && uncertainty.affected_region.target == "pkg.api"
            && uncertainty
                .effects
                .contains(&UncertaintyEffect::MayMutateNamespace)
    }));
    assert_eq!(unrelated.confidence, FindingConfidence::High);
}

#[test]
fn namespace_mapping_escape_is_a_local_review_blocker() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/mod.py",
        "def sink(mapping):\n    pass\n\ndef dead():\n    pass\n\nsink(locals())\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let dead = finding(&output, "dead");

    assert_eq!(dead.confidence, FindingConfidence::Review);
    assert!(dead.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::DynamicAttributeRead
            && uncertainty.affected_region.target == "pkg.mod"
    }));
    assert!(dead.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::NamespaceMutation
            && uncertainty.affected_region.target == "pkg.mod"
    }));
}

#[test]
fn bare_namespace_mapping_and_dir_calls_do_not_taint_findings() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/mod.py",
        "def dead():\n    pass\n\nglobals()\nlocals()\nvars()\ndir()\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let dead = finding(&output, "dead");

    assert_eq!(dead.confidence, FindingConfidence::High);
    assert!(dead.uncertainty.is_empty());
}

#[test]
fn runtime_annotation_introspection_is_a_local_review_blocker() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/mod.py",
        "from __future__ import annotations\nimport typing\n\nclass User:\n    pass\n\ndef dead():\n    pass\n\ntyping.get_type_hints(User)\n",
    );

    let output = check_project(temp.path(), ProjectMode::Application, false);
    let dead = finding(&output, "dead");

    assert_eq!(dead.confidence, FindingConfidence::Review);
    assert!(dead.uncertainty.iter().any(|uncertainty| {
        uncertainty.kind == FindingUncertaintyKind::RuntimeAnnotationIntrospection
            && uncertainty
                .effects
                .contains(&UncertaintyEffect::MayEvaluateAnnotations)
    }));
}

#[test]
fn candidate_ids_are_stable_across_repeated_runs() {
    let temp = tempfile::tempdir().unwrap();
    write_file(temp.path(), "src/pkg/__init__.py", "");
    write_file(
        temp.path(),
        "src/pkg/mod.py",
        "def first():\n    pass\n\nclass Second:\n    pass\n",
    );

    let first = debug_candidates(temp.path(), ProjectMode::Application, false);
    let second = debug_candidates(temp.path(), ProjectMode::Application, false);
    let first_ids = first
        .candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect::<Vec<_>>();
    let second_ids = second
        .candidates
        .iter()
        .map(|candidate| candidate.candidate_id.clone())
        .collect::<Vec<_>>();

    assert_eq!(first_ids, second_ids);
    assert!(first_ids.iter().all(|id| id.starts_with("CULL")));
}

#[test]
fn bounded_malformed_input_corpus_returns_structured_diagnostics_without_panic() {
    let cases: &[(&str, &[u8])] = &[
        ("bad_parse.py", b"def broken(:\n    pass\n"),
        ("bad_indent.py", b"if True:\npass\n"),
        ("bad_decode.py", b"# coding: ascii\nname = '\xE9'\n"),
    ];

    for (name, bytes) in cases {
        let temp = tempfile::tempdir().unwrap();
        write_file(temp.path(), "src/pkg/__init__.py", "");
        let path = temp.path().join("src/pkg").join(name);
        fs::write(path, bytes).unwrap();

        let output = check_project(temp.path(), ProjectMode::Application, true);
        assert_eq!(
            output.project_completeness.status,
            ProjectCompletenessStatus::Partial
        );
        assert!(output
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning));
    }
}
