use std::path::PathBuf;

use cull_core::{BindingKind, DebugBindingsOutput, DefinitionKind, ScopeId};
use cull_python::{analyze_debug_bindings, DebugBindingsOptions};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("part1a")
        .join(name)
}

fn analyze_fixture(name: &str) -> DebugBindingsOutput {
    analyze_debug_bindings(DebugBindingsOptions {
        project_root: fixture(name),
        source_roots: Vec::new(),
        target_python: None,
    })
    .unwrap()
}

fn normalized_json(mut output: DebugBindingsOutput) -> String {
    output.project_root = "<PROJECT_ROOT>".to_owned();
    format!("{}\n", serde_json::to_string_pretty(&output).unwrap())
}

#[test]
fn binding_inventory_snapshot_is_stable() {
    let first = normalized_json(analyze_fixture("bindings"));
    let second = normalized_json(analyze_fixture("bindings"));

    assert_eq!(first, second);
    assert_eq!(
        first,
        include_str!("snapshots/part1a_bindings_debug_bindings.json")
    );
}

#[test]
fn repeated_definitions_and_assignment_replacement_are_ordered() {
    let output = analyze_fixture("bindings");
    let parse_bindings = output
        .bindings
        .iter()
        .filter(|binding| {
            binding.module.as_u32() == 1 && binding.scope.as_u32() == 1 && binding.name == "parse"
        })
        .collect::<Vec<_>>();

    assert_eq!(parse_bindings.len(), 3);
    assert_eq!(parse_bindings[0].kind, BindingKind::FunctionDefinition);
    assert_eq!(parse_bindings[1].kind, BindingKind::FunctionDefinition);
    assert_eq!(parse_bindings[2].kind, BindingKind::Assignment);
    assert_eq!(parse_bindings[0].replaces, None);
    assert_eq!(parse_bindings[1].replaces, Some(parse_bindings[0].id));
    assert_eq!(parse_bindings[2].replaces, Some(parse_bindings[1].id));
}

#[test]
fn arena_invariants_hold() {
    let output = analyze_fixture("bindings");

    for (index, scope) in output.scopes.iter().enumerate() {
        assert_eq!(scope.id.as_u32() as usize, index);
        assert_eq!(
            output.contexts[scope.context.as_u32() as usize].scope,
            scope.id
        );
    }

    for (index, context) in output.contexts.iter().enumerate() {
        assert_eq!(context.id.as_u32() as usize, index);
        assert_eq!(
            output.scopes[context.scope.as_u32() as usize].context,
            context.id
        );
    }

    for (index, symbol) in output.symbols.iter().enumerate() {
        assert_eq!(symbol.id.as_u32() as usize, index);
        assert_eq!(
            output.scopes[symbol.scope.as_u32() as usize].id,
            symbol.scope
        );
    }

    for (index, binding) in output.bindings.iter().enumerate() {
        assert_eq!(binding.id.as_u32() as usize, index);
        assert_eq!(
            output.symbols[binding.symbol.as_u32() as usize].id,
            binding.symbol
        );
    }

    for (index, definition) in output.definitions.iter().enumerate() {
        assert_eq!(definition.id.as_u32() as usize, index);
        let binding = &output.bindings[definition.binding.as_u32() as usize];
        assert_eq!(binding.definition, Some(definition.id));
        assert!(
            matches!(
                binding.kind,
                BindingKind::FunctionDefinition | BindingKind::ClassDefinition
            ),
            "definition attached to non-definition binding: {binding:?}"
        );
    }

    for definition in output
        .definitions
        .iter()
        .filter(|definition| definition.reportable)
    {
        assert!(matches!(
            definition.kind,
            DefinitionKind::Function | DefinitionKind::Class
        ));
        let matching_bindings = output
            .bindings
            .iter()
            .filter(|binding| binding.definition == Some(definition.id))
            .count();
        assert_eq!(matching_bindings, 1);
    }
}

#[test]
fn class_scope_is_not_lexical_parent_of_method_scope() {
    let output = analyze_fixture("bindings");
    let class_scope = output
        .scopes
        .iter()
        .find(|scope| scope.name == "pkg.sample::Parser")
        .unwrap();
    let method_scope = output
        .scopes
        .iter()
        .find(|scope| scope.name == "pkg.sample::Parser.parse")
        .unwrap();
    let method_context = &output.contexts[method_scope.context.as_u32() as usize];

    assert_ne!(method_scope.parent, Some(class_scope.id));
    assert_eq!(method_scope.parent, Some(ScopeId::new(1)));
    assert_eq!(method_context.parent, Some(class_scope.context));
}
