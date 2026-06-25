use std::{collections::BTreeSet, path::PathBuf, process::Command};

use cull_core::{
    BindingKind, DebugReferencesOutput, LookupSemantics, ReferenceBindingState, ReferenceFact,
    Resolution, ScopeId, SymbolId, UnresolvedReason,
};
use cull_python::{analyze_debug_references, DebugReferencesOptions};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("references")
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

fn analyze_fixture(name: &str) -> DebugReferencesOutput {
    analyze_debug_references(DebugReferencesOptions {
        project_root: fixture(name),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap()
}

fn normalized_json(mut output: DebugReferencesOutput) -> String {
    output.project_root = "<PROJECT_ROOT>".to_owned();
    format!("{}\n", serde_json::to_string_pretty(&output).unwrap())
}

fn scope_id(output: &DebugReferencesOutput, name: &str) -> ScopeId {
    output
        .scopes
        .iter()
        .find(|scope| scope.name == name)
        .unwrap_or_else(|| panic!("missing scope {name}"))
        .id
}

fn module_scope_id(output: &DebugReferencesOutput, name: &str) -> ScopeId {
    output
        .modules
        .iter()
        .find(|module| module.name == name)
        .unwrap_or_else(|| panic!("missing module {name}"))
        .scope
}

fn symbol_id(output: &DebugReferencesOutput, scope: ScopeId, name: &str) -> SymbolId {
    output
        .symbols
        .iter()
        .find(|symbol| symbol.scope == scope && symbol.name == name)
        .unwrap_or_else(|| panic!("missing symbol {name} in scope {scope:?}"))
        .id
}

fn reference_at(output: &DebugReferencesOutput, start: u32) -> &ReferenceFact {
    output
        .references
        .iter()
        .find(|reference| reference.span.start == start)
        .unwrap_or_else(|| panic!("missing reference at {start}"))
}

fn references_in_scope<'a>(
    output: &'a DebugReferencesOutput,
    scope_name: &str,
    source_spelling: &str,
) -> Vec<&'a ReferenceFact> {
    let scope = scope_id(output, scope_name);
    output
        .references
        .iter()
        .filter(|reference| {
            reference.source_scope == scope && reference.source_spelling == source_spelling
        })
        .collect()
}

fn assert_resolved(reference: &ReferenceFact, expected: SymbolId) {
    assert_eq!(reference.lexical_target, Resolution::Resolved(expected));
}

#[test]
fn references_snapshot_is_stable() {
    let first = normalized_json(analyze_fixture("references"));
    let second = normalized_json(analyze_fixture("references"));

    assert_eq!(first, second);
    assert_eq!(
        first,
        include_str!("snapshots/references_debug_references.json")
    );
}

#[test]
fn every_reference_has_flow_binding_state() {
    let output = analyze_fixture("references");

    assert!(!output.references.is_empty());
    assert!(output
        .references
        .iter()
        .all(|reference| reference.binding_state != ReferenceBindingState::NotApplicable));
}

#[test]
fn references_use_their_execution_context() {
    let output = analyze_fixture("references");
    let module_context = output
        .modules
        .iter()
        .find(|module| module.name == "pkg.refs")
        .unwrap()
        .context;
    let function_context = output
        .scopes
        .iter()
        .find(|scope| scope.name == "pkg.refs::with_default")
        .unwrap()
        .context;
    let class_context = output
        .scopes
        .iter()
        .find(|scope| scope.name == "pkg.refs::ContextCase")
        .unwrap()
        .context;
    let comprehension_context = output
        .scopes
        .iter()
        .find(|scope| scope.name == "pkg.refs::ContextCase::<comp@423>")
        .unwrap()
        .context;

    assert_eq!(reference_at(&output, 134).source_context, module_context);
    assert_eq!(reference_at(&output, 167).source_context, module_context);
    assert_eq!(reference_at(&output, 195).source_context, function_context);
    assert_eq!(reference_at(&output, 281).source_context, module_context);
    assert_eq!(reference_at(&output, 302).source_context, module_context);
    assert_eq!(reference_at(&output, 330).source_context, class_context);
    assert_eq!(reference_at(&output, 396).source_context, class_context);
    assert_eq!(
        reference_at(&output, 424).source_context,
        comprehension_context
    );
}

#[test]
fn class_methods_and_comprehensions_do_not_capture_class_locals() {
    let output = analyze_fixture("references");
    let module_scope = module_scope_id(&output, "pkg.refs");
    let class_scope = scope_id(&output, "pkg.refs::ContextCase");

    let module_x = symbol_id(&output, module_scope, "x");
    let class_private = symbol_id(&output, class_scope, "_ContextCase__private");
    let module_private = symbol_id(&output, module_scope, "_ContextCase__private");
    let module_class_value = symbol_id(&output, module_scope, "class_value");

    assert_resolved(reference_at(&output, 424), module_x);
    assert_resolved(reference_at(&output, 465), class_private);
    assert_resolved(reference_at(&output, 513), module_private);
    assert_resolved(reference_at(&output, 525), module_class_value);
}

#[test]
fn global_nonlocal_and_free_references_match_cpython_symtable_intent() {
    let source = fixture("declarations").join("src/pkg/scopes.py");
    let oracle = Command::new("python3")
        .arg(workspace_root().join("crates/cull-python/tests/support/cpython_definition_oracle.py"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(oracle.status.success());

    let oracle: serde_json::Value = serde_json::from_slice(&oracle.stdout).unwrap();
    let tables = oracle["symtable"]["tables"].as_array().unwrap();
    let uses_free_table = tables
        .iter()
        .find(|table| table["name"] == "uses_free")
        .unwrap();
    let uses_free_symbols = uses_free_table["symbols"].as_array().unwrap();
    let outer_value = uses_free_symbols
        .iter()
        .find(|symbol| symbol["name"] == "outer_value")
        .unwrap();
    let module_value = uses_free_symbols
        .iter()
        .find(|symbol| symbol["name"] == "module_value")
        .unwrap();
    let missing_global = uses_free_symbols
        .iter()
        .find(|symbol| symbol["name"] == "missing_global")
        .unwrap();

    assert_eq!(outer_value["is_free"], true);
    assert_eq!(module_value["is_global"], true);
    assert_eq!(missing_global["is_global"], true);

    let output = analyze_fixture("declarations");
    let module_scope = module_scope_id(&output, "pkg.scopes");
    let outer_scope = scope_id(&output, "pkg.scopes::outer");
    let outer_value_symbol = symbol_id(&output, outer_scope, "outer_value");
    let module_value_symbol = symbol_id(&output, module_scope, "module_value");
    let missing_global_symbol = symbol_id(&output, module_scope, "missing_global");

    assert_resolved(
        references_in_scope(&output, "pkg.scopes::outer.uses_free", "outer_value")[0],
        outer_value_symbol,
    );
    assert_resolved(
        references_in_scope(&output, "pkg.scopes::outer.uses_free", "module_value")[0],
        module_value_symbol,
    );
    assert_resolved(
        references_in_scope(&output, "pkg.scopes::outer.uses_free", "missing_global")[0],
        missing_global_symbol,
    );

    let nested_outer_value =
        references_in_scope(&output, "pkg.scopes::outer.Nested", "outer_value");
    assert_resolved(nested_outer_value[0], outer_value_symbol);

    let global_binding = output
        .bindings
        .iter()
        .find(|binding| {
            binding.kind == BindingKind::Assignment
                && binding.name == "module_value"
                && binding.scope == module_scope
        })
        .unwrap();
    assert_eq!(global_binding.scope, module_scope);

    let nonlocal_binding = output
        .bindings
        .iter()
        .find(|binding| {
            binding.kind == BindingKind::Assignment
                && binding.name == "outer_value"
                && binding.scope == outer_scope
                && binding.range.start == 329
        })
        .unwrap();
    assert_eq!(nonlocal_binding.scope, outer_scope);

    let missing_global_ref =
        references_in_scope(&output, "pkg.scopes::outer.uses_free", "missing_global")[0];
    assert_eq!(
        missing_global_ref.lookup,
        LookupSemantics::GlobalThenBuiltin {
            global_symbol: missing_global_symbol
        }
    );
}

#[test]
fn invalid_global_and_nonlocal_declarations_are_explicit() {
    let output = analyze_fixture("invalid_declarations");
    let codes = output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.as_str())
        .collect::<BTreeSet<_>>();

    assert!(codes.contains("CULL_P1100"));
    assert!(codes.contains("CULL_P1101"));
    assert!(codes.contains("CULL_P1102"));
    assert!(codes.contains("CULL_P1103"));
    assert!(codes.contains("CULL_P1104"));

    for reference in references_in_scope(&output, "pkg.bad::use_before_global", "value") {
        assert_eq!(
            reference.lexical_target,
            Resolution::Unresolved(UnresolvedReason::InvalidGlobalDeclaration)
        );
    }

    let missing_nonlocal = references_in_scope(&output, "pkg.bad::missing_nonlocal", "absent");
    assert_eq!(
        missing_nonlocal[0].lexical_target,
        Resolution::Unresolved(UnresolvedReason::MissingNonlocalBinding)
    );

    let conflicting = references_in_scope(&output, "pkg.bad::conflicting", "both");
    assert_eq!(
        conflicting[0].lexical_target,
        Resolution::Unresolved(UnresolvedReason::ConflictingDeclaration)
    );
}
