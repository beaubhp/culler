use std::{collections::BTreeSet, env, fs, path::PathBuf, process::Command};

use cull_core::{
    AnnotationEvaluation, BindingKind, DefinitionEffectKind, DefinitionRole,
    InternalCandidateDisposition, InternalCandidateReason, PythonVersion, ReferenceBindingState,
    ReferenceFact, ReferencePhase, ReferenceRole, ResidualLookup, ScopeKind,
};
use cull_python::{analyze_debug_references, DebugReferencesOptions};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("annotation_evidence")
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

fn analyze_fixture(name: &str) -> cull_core::DebugReferencesOutput {
    analyze_debug_references(DebugReferencesOptions {
        project_root: fixture(name),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap()
}

fn analyze_temp_source(
    source: &str,
    target_python: PythonVersion,
) -> cull_core::DebugReferencesOutput {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("src/pkg");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        temp.path().join("pyproject.toml"),
        "[tool.cull]\nsrc = \"src\"\n",
    )
    .unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    fs::write(package.join("module.py"), source).unwrap();

    analyze_debug_references(DebugReferencesOptions {
        project_root: temp.path().to_path_buf(),
        source_roots: Vec::new(),
        target_python: Some(target_python),
    })
    .unwrap()
}

fn normalized_json(mut output: cull_core::DebugReferencesOutput) -> String {
    output.project_root = "<PROJECT_ROOT>".to_owned();
    format!("{}\n", serde_json::to_string_pretty(&output).unwrap())
}

fn references_named<'a>(
    output: &'a cull_core::DebugReferencesOutput,
    source_spelling: &str,
) -> Vec<&'a ReferenceFact> {
    let mut references = output
        .references
        .iter()
        .filter(|reference| reference.source_spelling == source_spelling)
        .collect::<Vec<_>>();
    references.sort_by_key(|reference| reference.span.start);
    references
}

fn binding_kinds_for_reference(
    output: &cull_core::DebugReferencesOutput,
    reference: &ReferenceFact,
) -> BTreeSet<BindingKind> {
    let symbols = match &reference.lexical_target {
        cull_core::Resolution::Resolved(symbol) => vec![*symbol],
        cull_core::Resolution::Ambiguous(symbols) => symbols.clone(),
        cull_core::Resolution::External | cull_core::Resolution::Unresolved(_) => Vec::new(),
    };
    output
        .bindings
        .iter()
        .filter(|binding| symbols.contains(&binding.symbol))
        .map(|binding| binding.kind)
        .collect()
}

fn cpython_314() -> Option<PathBuf> {
    let candidate = env::var_os("CULL_CPYTHON_3_14")
        .or_else(|| env::var_os("PYTHON_3_14"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("python3"));
    let output = Command::new(&candidate)
        .arg("-c")
        .arg("import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
        .output()
        .ok()?;
    if !output.status.success() {
        eprintln!(
            "skipping CPython 3.14 oracle test: `{}` did not run",
            candidate.display()
        );
        return None;
    }
    let version = String::from_utf8_lossy(&output.stdout);
    if version.trim() == "3.14" {
        Some(candidate)
    } else {
        eprintln!(
            "skipping CPython 3.14 oracle test: `{}` is Python {}",
            candidate.display(),
            version.trim()
        );
        None
    }
}

#[test]
fn annotation_evidence_snapshot_is_stable() {
    let first = normalized_json(analyze_fixture("final"));
    let second = normalized_json(analyze_fixture("final"));

    assert_eq!(first, second);
    assert_eq!(
        first,
        include_str!("snapshots/annotation_evidence_debug_references.json")
    );
}

#[test]
fn cpython_oracle_exposes_annotation_scope_facts() {
    let Some(python) = cpython_314() else {
        return;
    };
    let source = fixture("final").join("src/pkg/annotations.py");
    let oracle = Command::new(python)
        .arg(workspace_root().join("crates/cull-python/tests/support/cpython_definition_oracle.py"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(oracle.status.success());

    let oracle: serde_json::Value = serde_json::from_slice(&oracle.stdout).unwrap();
    let tables = oracle["symtable"]["tables"].as_array().unwrap();
    assert!(tables
        .iter()
        .any(|table| { table["name"] == "GenericBox" && table["type"] == "type parameters" }));
    assert!(tables
        .iter()
        .any(|table| { table["name"] == "Alias" && table["type"] == "type parameters" }));

    let class_annotation = tables
        .iter()
        .find(|table| table["name"] == "__annotate__" && table["line"] == 46)
        .expect("missing CPython class annotation table");
    let class_annotation_symbols = class_annotation["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert!(class_annotation_symbols.contains("T"));
    assert!(class_annotation_symbols.contains("__classdict__"));

    let output = analyze_fixture("final");
    assert!(output
        .scopes
        .iter()
        .any(|scope| { scope.kind == ScopeKind::Annotation && scope.name.contains(":class>") }));
    assert!(output
        .scopes
        .iter()
        .any(|scope| { scope.kind == ScopeKind::Annotation && scope.name.contains("type-alias") }));
}

#[test]
fn annotations_are_site_aware_and_string_refs_are_type_only() {
    let output = analyze_fixture("final");

    let string_user = references_named(&output, "User")
        .into_iter()
        .find(|reference| reference.phase == ReferencePhase::TypeOnly)
        .expect("missing string User annotation reference");
    assert_eq!(string_user.role, ReferenceRole::Annotation);
    assert_eq!(
        string_user
            .annotation_semantics
            .expect("missing annotation semantics")
            .phase,
        ReferencePhase::TypeOnly
    );

    let lazy_user = references_named(&output, "User")
        .into_iter()
        .find(|reference| reference.phase == ReferencePhase::LazyAnnotation)
        .expect("missing lazy User annotation reference");
    assert_eq!(lazy_user.role, ReferenceRole::Annotation);

    assert!(references_named(&output, "active").is_empty());
}

#[test]
fn target_version_controls_annotation_evaluation_semantics() {
    let eager = analyze_temp_source(
        r#"
class User:
    pass

def consume(value: User) -> None:
    return None
"#,
        PythonVersion::PY311,
    );
    let eager_user = references_named(&eager, "User")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Annotation)
        .expect("missing eager User annotation");
    let eager_semantics = eager_user.annotation_semantics.unwrap();
    assert_eq!(eager_user.phase, ReferencePhase::DefinitionTime);
    assert_eq!(eager_semantics.evaluation, AnnotationEvaluation::Eager);

    let stringified = analyze_temp_source(
        r#"
from __future__ import annotations

class User:
    pass

def consume(value: User) -> None:
    return None
"#,
        PythonVersion::PY314,
    );
    let stringified_user = references_named(&stringified, "User")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Annotation)
        .expect("missing stringified User annotation");
    let stringified_semantics = stringified_user.annotation_semantics.unwrap();
    assert_eq!(stringified_user.phase, ReferencePhase::TypeOnly);
    assert_eq!(
        stringified_semantics.evaluation,
        AnnotationEvaluation::Stringified
    );

    let deferred_bound = analyze_temp_source(
        r#"
class User:
    pass

type Alias[T: User] = list[T]
"#,
        PythonVersion::PY312,
    );
    let bound_user = references_named(&deferred_bound, "User")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Annotation)
        .expect("missing deferred bound User annotation");
    let bound_semantics = bound_user.annotation_semantics.unwrap();
    assert_eq!(bound_user.phase, ReferencePhase::LazyAnnotation);
    assert_eq!(bound_semantics.evaluation, AnnotationEvaluation::Deferred);
    let alias_t = references_named(&deferred_bound, "T")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Annotation)
        .expect("missing alias T annotation");
    assert!(
        binding_kinds_for_reference(&deferred_bound, alias_t).contains(&BindingKind::TypeParameter)
    );

    let deferred_default = analyze_temp_source(
        r#"
class User:
    pass

type Alias[T = User] = list[T]
"#,
        PythonVersion::PY313,
    );
    let default_user = references_named(&deferred_default, "User")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Annotation)
        .expect("missing deferred default User annotation");
    let default_semantics = default_user.annotation_semantics.unwrap();
    assert_eq!(default_user.phase, ReferencePhase::LazyAnnotation);
    assert_eq!(default_semantics.evaluation, AnnotationEvaluation::Deferred);
}

#[test]
fn type_checking_origin_overloads_and_candidates_are_recorded() {
    let output = analyze_fixture("final");

    let type_only_import = references_named(&output, "TypeOnlyImport")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Value)
        .expect("missing TYPE_CHECKING branch value reference");
    assert_eq!(type_only_import.phase, ReferencePhase::TypeOnly);

    let test_module = output
        .modules
        .iter()
        .find(|module| module.name == "test_annotations")
        .expect("missing test module");
    assert_eq!(test_module.origin_domain, cull_core::OriginDomain::Test);
    assert_eq!(
        test_module.origin_evidence,
        cull_core::OriginEvidence::CullConfiguration
    );

    let overloaded = output
        .definitions
        .iter()
        .filter(|definition| definition.name == "overloaded")
        .collect::<Vec<_>>();
    assert_eq!(overloaded.len(), 3);
    assert_eq!(
        overloaded
            .iter()
            .filter(|definition| definition.role == DefinitionRole::OverloadDeclaration)
            .count(),
        2
    );
    assert_eq!(output.overload_groups.len(), 1);

    let unreferenced = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.annotations::unreferenced")
        .expect("missing unreferenced definition");
    let candidate = output
        .internal_candidates
        .iter()
        .find(|candidate| candidate.definition == unreferenced.id)
        .expect("missing internal candidate");
    assert_eq!(
        candidate.disposition,
        InternalCandidateDisposition::Candidate
    );
}

#[test]
fn type_checking_and_overload_provenance_are_scope_bound() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("src/pkg");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        temp.path().join("pyproject.toml"),
        "[tool.cull]\nsrc = \"src\"\ntarget-python = \"3.14\"\n",
    )
    .unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    fs::write(
        package.join("scoped.py"),
        r#"
class User:
    pass


def imports_type_checking_locally():
    from typing import TYPE_CHECKING as LOCAL_TC
    if LOCAL_TC:
        local_ref = User


if LOCAL_TC:
    leaked_ref = User


def imports_overload_locally():
    from typing import overload as local_overload


@local_overload
def leaked(value):
    return value
"#,
    )
    .unwrap();

    let output = analyze_debug_references(DebugReferencesOptions {
        project_root: temp.path().to_path_buf(),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap();

    let user_references = references_named(&output, "User");
    assert!(user_references
        .iter()
        .any(|reference| reference.phase == ReferencePhase::TypeOnly));
    assert!(user_references
        .iter()
        .any(|reference| reference.phase == ReferencePhase::DefinitionTime));

    let leaked = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.scoped::leaked")
        .expect("missing leaked definition");
    assert_eq!(leaked.role, DefinitionRole::Normal);
    assert!(leaked.reportable);
}

#[test]
fn type_checking_definitions_are_non_reportable() {
    let output = analyze_temp_source(
        r#"
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    def only_for_types():
        return None
"#,
        PythonVersion::PY314,
    );

    let definition = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.module::only_for_types")
        .expect("missing type-checking definition");
    assert!(!definition.reportable);

    let candidate = output
        .internal_candidates
        .iter()
        .find(|candidate| candidate.definition == definition.id)
        .expect("missing type-checking candidate");
    assert_eq!(
        candidate.disposition,
        InternalCandidateDisposition::Suppressed
    );
    assert!(candidate
        .reasons
        .contains(&InternalCandidateReason::NonReportableDefinition));
}

#[test]
fn literal_annotation_provenance_handles_aliases_and_shadowing() {
    let output = analyze_temp_source(
        r#"
from typing import Literal as LiteralAlias

class Hidden:
    pass

def skipped(value: LiteralAlias["Hidden"]) -> None:
    return None

LiteralAlias = list

def visible(value: LiteralAlias[Hidden]) -> None:
    return None
"#,
        PythonVersion::PY314,
    );

    let hidden_references = references_named(&output, "Hidden");
    assert_eq!(hidden_references.len(), 1);
    assert!(hidden_references
        .iter()
        .all(|reference| reference.role == ReferenceRole::Annotation));
}

#[test]
fn class_type_parameters_resolve_in_class_and_method_annotations() {
    let output = analyze_temp_source(
        r#"
class Box[T]:
    item: T

    def get(self, value: T) -> T:
        return value
"#,
        PythonVersion::PY314,
    );

    let type_parameter_refs = references_named(&output, "T")
        .into_iter()
        .filter(|reference| reference.role == ReferenceRole::Annotation)
        .collect::<Vec<_>>();
    assert_eq!(type_parameter_refs.len(), 3);
    for reference in type_parameter_refs {
        assert!(
            binding_kinds_for_reference(&output, reference).contains(&BindingKind::TypeParameter)
        );
    }
}

#[test]
fn residual_global_lookup_suppresses_internal_candidates() {
    let output = analyze_temp_source(
        r#"
def caller():
    used_late()

def used_late():
    return None
"#,
        PythonVersion::PY314,
    );

    let reference = references_named(&output, "used_late")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Value)
        .expect("missing forward call reference");
    match &reference.binding_state {
        ReferenceBindingState::Analyzed(state) => {
            assert_eq!(state.residual, ResidualLookup::RuntimeGlobalThenBuiltin);
        }
        state => panic!("expected analyzed residual state, got {state:?}"),
    }

    let definition = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.module::used_late")
        .expect("missing used_late definition");
    let candidate = output
        .internal_candidates
        .iter()
        .find(|candidate| candidate.definition == definition.id)
        .expect("missing used_late candidate");
    assert_eq!(
        candidate.disposition,
        InternalCandidateDisposition::Suppressed
    );
    assert!(candidate
        .reasons
        .contains(&InternalCandidateReason::UnresolvedOrUnsupportedReference));
}

#[test]
fn metaclass_references_drive_definition_effects() {
    let output = analyze_temp_source(
        r#"
class Meta(type):
    pass

class UsesMeta(metaclass=Meta):
    pass
"#,
        PythonVersion::PY314,
    );

    let metaclass_reference = references_named(&output, "Meta")
        .into_iter()
        .find(|reference| reference.role == ReferenceRole::Metaclass)
        .expect("missing metaclass reference");
    assert_eq!(metaclass_reference.phase, ReferencePhase::DefinitionTime);

    let definition = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.module::UsesMeta")
        .expect("missing UsesMeta definition");
    let effect_set = output
        .definition_effect_sets
        .iter()
        .find(|set| set.id == definition.definition_effects)
        .expect("missing UsesMeta effect set")
        .effects
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert!(effect_set.contains(&DefinitionEffectKind::MetaclassEvaluation));
}

#[test]
fn overload_declarations_without_implementation_emit_diagnostic() {
    let output = analyze_temp_source(
        r#"
from typing import overload

@overload
def missing(value: int) -> int:
    ...

@overload
def missing(value: str) -> str:
    ...
"#,
        PythonVersion::PY314,
    );

    assert!(output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "CULL_P1107"));
    let group = output
        .overload_groups
        .iter()
        .find(|group| group.name == "missing")
        .expect("missing overload group");
    assert!(group.implementation.is_none());

    let declarations = output
        .definitions
        .iter()
        .filter(|definition| definition.name == "missing")
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);
    assert!(declarations.iter().all(|definition| {
        definition.role == DefinitionRole::OverloadDeclaration && !definition.reportable
    }));
}

#[test]
fn unsupported_string_annotations_fail_closed_for_internal_candidates() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("src/pkg");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        temp.path().join("pyproject.toml"),
        "[tool.cull]\nsrc = \"src\"\ntarget-python = \"3.14\"\n",
    )
    .unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    fs::write(
        package.join("bad_annotation.py"),
        r#"
class MaybeReferenced:
    pass


def uses_bad_annotation(value: "MaybeReferenced |") -> None:
    return None
"#,
    )
    .unwrap();

    let output = analyze_debug_references(DebugReferencesOptions {
        project_root: temp.path().to_path_buf(),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap();

    assert!(output
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "CULL_P1106"));
    assert!(output.references.iter().any(|reference| {
        matches!(
            reference.lexical_target,
            cull_core::Resolution::Unresolved(cull_core::UnresolvedReason::UnsupportedAnnotation)
        )
    }));

    let definition = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.bad_annotation::MaybeReferenced")
        .expect("missing MaybeReferenced definition");
    let candidate = output
        .internal_candidates
        .iter()
        .find(|candidate| candidate.definition == definition.id)
        .expect("missing MaybeReferenced candidate");
    assert_eq!(
        candidate.disposition,
        InternalCandidateDisposition::Suppressed
    );
    assert!(candidate
        .reasons
        .contains(&InternalCandidateReason::UnresolvedOrUnsupportedReference));
}

#[test]
fn definition_effects_and_removal_risk_are_structural() {
    let output = analyze_fixture("final");
    let eager = output
        .definitions
        .iter()
        .find(|definition| definition.qualified_name == "pkg.annotations::eager_annotation")
        .expect("missing eager_annotation definition");
    let effect_set = output
        .definition_effect_sets
        .iter()
        .find(|set| set.id == eager.definition_effects)
        .expect("missing definition effect set")
        .effects
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    assert!(effect_set.contains(&DefinitionEffectKind::DecoratorApplication));
    assert!(effect_set.contains(&DefinitionEffectKind::DefaultExpressionEvaluation));
    assert!(effect_set.contains(&DefinitionEffectKind::LazyAnnotationIntrospectionRisk));
}
