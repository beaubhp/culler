use std::{collections::BTreeSet, path::PathBuf};

use cull_core::{
    BindingId, BindingKind, BindingState, DebugReferencesOutput, FlowUncertaintyKind,
    LocalReachability, ReferenceBindingState, ReferenceFact, ResidualLookup,
};
use cull_python::{analyze_debug_references, DebugReferencesOptions};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("part1c")
        .join(name)
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

fn references_named<'a>(
    output: &'a DebugReferencesOutput,
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

fn analyzed(reference: &ReferenceFact) -> &BindingState {
    match &reference.binding_state {
        ReferenceBindingState::Analyzed(state) => state,
        other => panic!("expected analyzed binding state, got {other:?}"),
    }
}

fn binding_set(output: &DebugReferencesOutput, state: &BindingState) -> BTreeSet<BindingId> {
    output
        .binding_sets
        .iter()
        .find(|set| set.id == state.bindings)
        .unwrap_or_else(|| panic!("missing binding set {:?}", state.bindings))
        .bindings
        .iter()
        .copied()
        .collect()
}

fn uncertainty_set(
    output: &DebugReferencesOutput,
    state: &BindingState,
) -> BTreeSet<FlowUncertaintyKind> {
    output
        .flow_uncertainty_sets
        .iter()
        .find(|set| set.id == state.uncertainty)
        .unwrap_or_else(|| panic!("missing uncertainty set {:?}", state.uncertainty))
        .uncertainties
        .iter()
        .copied()
        .collect()
}

fn binding_ids(output: &DebugReferencesOutput, name: &str, kind: BindingKind) -> Vec<BindingId> {
    let mut bindings = output
        .bindings
        .iter()
        .filter(|binding| binding.name == name && binding.kind == kind)
        .collect::<Vec<_>>();
    bindings.sort_by_key(|binding| binding.order);
    bindings.into_iter().map(|binding| binding.id).collect()
}

fn singleton(binding: BindingId) -> BTreeSet<BindingId> {
    BTreeSet::from([binding])
}

#[test]
fn flow_snapshot_is_stable() {
    let first = normalized_json(analyze_fixture("flow"));
    let second = normalized_json(analyze_fixture("flow"));

    assert_eq!(first, second);
    assert_eq!(
        first,
        include_str!("snapshots/part1c_flow_debug_references.json")
    );
}

#[test]
fn straight_line_redefinitions_partial_branches_and_deletes_are_exact() {
    let output = analyze_fixture("flow");

    let module_partial_binding = binding_ids(&output, "module_partial", BindingKind::Assignment)[0];
    let module_partial_state = analyzed(references_named(&output, "module_partial")[0]);
    assert_eq!(
        binding_set(&output, module_partial_state),
        singleton(module_partial_binding)
    );
    assert_eq!(
        module_partial_state.residual,
        ResidualLookup::BuiltinOrNameError
    );

    let seq_bindings = binding_ids(&output, "seq_value", BindingKind::Assignment);
    let seq_refs = references_named(&output, "seq_value");
    assert_eq!(
        binding_set(&output, analyzed(seq_refs[0])),
        singleton(seq_bindings[0])
    );
    assert_eq!(
        binding_set(&output, analyzed(seq_refs[1])),
        singleton(seq_bindings[1])
    );

    let branch_bindings = binding_ids(&output, "branch_value", BindingKind::Assignment);
    let branch_state = analyzed(references_named(&output, "branch_value")[0]);
    assert_eq!(
        binding_set(&output, branch_state),
        branch_bindings.into_iter().collect()
    );
    assert_eq!(branch_state.residual, ResidualLookup::None);

    let partial_binding = binding_ids(&output, "partial_value", BindingKind::Assignment)[0];
    let partial_state = analyzed(references_named(&output, "partial_value")[0]);
    assert_eq!(
        binding_set(&output, partial_state),
        singleton(partial_binding)
    );
    assert_eq!(partial_state.residual, ResidualLookup::UnboundLocal);

    let deleted_state = analyzed(references_named(&output, "deleted_value")[0]);
    assert!(binding_set(&output, deleted_state).is_empty());
    assert_eq!(deleted_state.residual, ResidualLookup::UnboundLocal);
}

#[test]
fn unreachable_and_same_invocation_global_flow_are_explicit() {
    let output = analyze_fixture("flow");

    let global_refs = references_named(&output, "global_slot");
    let unreachable_state = analyzed(
        global_refs
            .iter()
            .find(|reference| analyzed(reference).reachability == LocalReachability::Unreachable)
            .copied()
            .unwrap(),
    );
    assert_eq!(
        unreachable_state.reachability,
        LocalReachability::Unreachable
    );
    assert!(binding_set(&output, unreachable_state).is_empty());

    let global_assignment = binding_ids(&output, "global_slot", BindingKind::Assignment)[1];
    let known_state = analyzed(global_refs[1]);
    assert_eq!(
        binding_set(&output, known_state),
        singleton(global_assignment)
    );
    assert_eq!(known_state.residual, ResidualLookup::None);

    let after_call_state = analyzed(global_refs[2]);
    assert_eq!(
        binding_set(&output, after_call_state),
        singleton(global_assignment)
    );
    assert_eq!(
        after_call_state.residual,
        ResidualLookup::RuntimeGlobalThenBuiltin
    );
    assert!(uncertainty_set(&output, after_call_state)
        .contains(&FlowUncertaintyKind::OpaqueCallMayMutateGlobal));

    let conditional_global_assignment =
        binding_ids(&output, "global_slot", BindingKind::Assignment)
            .into_iter()
            .last()
            .unwrap();
    let conditional_global = analyzed(global_refs[3]);
    assert_eq!(
        binding_set(&output, conditional_global),
        singleton(conditional_global_assignment)
    );
    assert_eq!(
        conditional_global.residual,
        ResidualLookup::RuntimeGlobalThenBuiltin
    );

    let nonlocal_assignment = binding_ids(&output, "outer_value", BindingKind::Assignment)
        .into_iter()
        .last()
        .unwrap();
    let nonlocal_state = analyzed(references_named(&output, "outer_value")[0]);
    assert_eq!(
        binding_set(&output, nonlocal_state),
        singleton(nonlocal_assignment)
    );
    assert_eq!(nonlocal_state.residual, ResidualLookup::RuntimeFreeVariable);
}

#[test]
fn exceptional_and_match_flow_preserve_concrete_candidates_with_uncertainty() {
    let output = analyze_fixture("flow");

    let exc_binding = binding_ids(&output, "exc_value", BindingKind::ExceptTarget)[0];
    let exc_refs = references_named(&output, "exc_value");
    let inside_exc = analyzed(exc_refs[0]);
    assert_eq!(binding_set(&output, inside_exc), singleton(exc_binding));
    assert_eq!(inside_exc.residual, ResidualLookup::None);

    let after_exc = analyzed(exc_refs[1]);
    assert!(binding_set(&output, after_exc).is_empty());
    assert_eq!(after_exc.residual, ResidualLookup::UnboundLocal);

    let try_binding = binding_ids(&output, "try_value", BindingKind::Assignment)[0];
    let try_state = analyzed(references_named(&output, "try_value")[0]);
    assert_eq!(binding_set(&output, try_state), singleton(try_binding));
    assert_eq!(try_state.residual, ResidualLookup::UnboundLocal);
    assert!(
        uncertainty_set(&output, try_state).contains(&FlowUncertaintyKind::ComplexExceptionFlow)
    );

    let captured_binding = binding_ids(&output, "captured_value", BindingKind::MatchCapture)[0];
    let captured_refs = references_named(&output, "captured_value");
    assert_eq!(
        binding_set(&output, analyzed(captured_refs[0])),
        singleton(captured_binding)
    );
    let captured_after = analyzed(captured_refs[1]);
    assert_eq!(
        binding_set(&output, captured_after),
        singleton(captured_binding)
    );
    assert_eq!(captured_after.residual, ResidualLookup::UnboundLocal);
    assert!(
        uncertainty_set(&output, captured_after).contains(&FlowUncertaintyKind::FailedPartialMatch)
    );
}

#[test]
fn class_fallback_and_comprehension_deferred_execution_are_lookup_aware() {
    let output = analyze_fixture("flow");

    let maybe_class_binding = binding_ids(&output, "maybe_class", BindingKind::Assignment)[0];
    let maybe_class = analyzed(references_named(&output, "maybe_class")[0]);
    assert_eq!(
        binding_set(&output, maybe_class),
        singleton(maybe_class_binding)
    );
    assert_eq!(
        maybe_class.residual,
        ResidualLookup::RuntimeGlobalThenBuiltin
    );
    assert!(uncertainty_set(&output, maybe_class)
        .contains(&FlowUncertaintyKind::OpaqueCallMayMutateGlobal));

    let eager_binding = binding_ids(&output, "eager_source", BindingKind::Assignment)[0];
    let eager_refs = references_named(&output, "eager_source");
    let eager_state = analyzed(eager_refs[0]);
    assert_eq!(binding_set(&output, eager_state), singleton(eager_binding));

    let generator_state = analyzed(eager_refs[1]);
    assert!(binding_set(&output, generator_state).is_empty());
    assert_eq!(
        generator_state.residual,
        ResidualLookup::RuntimeGlobalThenBuiltin
    );
}
