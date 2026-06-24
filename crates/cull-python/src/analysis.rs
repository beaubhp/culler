use std::{fs, path::PathBuf};

use cull_core::{
    DebugBindingModule, DebugBindingsOutput, DebugDefinition, DebugDefinitionsOutput, DebugModule,
    DebugReferencesOutput, Diagnostic, PythonVersion, SemanticGraph, SemanticGraphBuilder,
    SourceRootOutput,
};

use crate::{
    decode_python_source,
    discovery::{discover_project, DiscoveryOptions},
    flow_analysis::analyze_module_flow,
    frontend::{ParseInput, PythonFrontend},
    ruff_frontend::{parse_ruff_module, RuffFrontend},
    semantic_inventory::collect_module_semantics,
};

#[derive(Clone, Debug)]
pub struct DebugDefinitionsOptions {
    pub project_root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub target_python: Option<PythonVersion>,
}

#[derive(Clone, Debug)]
pub struct DebugBindingsOptions {
    pub project_root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub target_python: Option<PythonVersion>,
}

#[derive(Clone, Debug)]
pub struct DebugReferencesOptions {
    pub project_root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub target_python: Option<PythonVersion>,
}

pub fn analyze_debug_definitions(
    options: DebugDefinitionsOptions,
) -> Result<DebugDefinitionsOutput, Diagnostic> {
    let project = discover_project(DiscoveryOptions {
        project_root: options.project_root,
        explicit_source_roots: options.source_roots,
        target_python: options.target_python,
    })
    .map_err(|error| Diagnostic::error("CULL_P0000", error.to_string()))?;

    let frontend = RuffFrontend;
    let mut diagnostics = project.diagnostics.clone();
    let mut modules = Vec::new();

    for module in &project.modules {
        let bytes = match fs::read(&module.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(
                    Diagnostic::error("CULL_P0100", format!("failed to read source: {error}"))
                        .with_path(module.display_path.clone()),
                );
                continue;
            }
        };

        let source = match decode_python_source(&bytes) {
            Ok(source) => source,
            Err(error) => {
                diagnostics.push(
                    Diagnostic::error("CULL_P0101", error.to_string())
                        .with_path(module.display_path.clone()),
                );
                continue;
            }
        };

        match frontend.parse_module(ParseInput {
            file_id: module.file,
            module_id: module.id,
            module_name: &module.name,
            display_path: &module.display_path,
            source: &source,
            target_python: project.target_python,
        }) {
            Ok(parsed) => modules.push(DebugModule {
                name: parsed.module.name,
                path: parsed.module.path,
                future_annotations: parsed.module.future_annotations,
                definitions: parsed
                    .module
                    .definitions
                    .into_iter()
                    .map(|definition| DebugDefinition {
                        kind: definition.kind,
                        name: definition.name,
                        range: definition.range,
                        name_range: definition.name_range,
                        is_async: definition.is_async,
                        decorator_count: definition.decorator_count,
                        type_parameter_count: definition.type_parameter_count,
                    })
                    .collect(),
            }),
            Err(mut parse_diagnostics) => diagnostics.append(&mut parse_diagnostics),
        }
    }

    modules.sort_by(|left, right| left.name.cmp(&right.name).then(left.path.cmp(&right.path)));
    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(
                left.range
                    .as_ref()
                    .map(|range| range.start)
                    .cmp(&right.range.as_ref().map(|range| range.start)),
            )
            .then(left.code.cmp(&right.code))
            .then(left.message.cmp(&right.message))
    });

    Ok(DebugDefinitionsOutput {
        schema_version: 1,
        target_python: project.target_python,
        project_root: crate::paths::slash_path(&project.project_root),
        source_roots: project.source_root_output(),
        modules,
        diagnostics,
    })
}

pub fn analyze_debug_bindings(
    options: DebugBindingsOptions,
) -> Result<DebugBindingsOutput, Diagnostic> {
    let output = analyze_semantic_project(
        options.project_root,
        options.source_roots,
        options.target_python,
    )?;
    let SemanticDebugData {
        target_python,
        project_root,
        source_roots,
        graph,
        diagnostics,
    } = output;

    Ok(DebugBindingsOutput {
        schema_version: 1,
        target_python,
        project_root,
        source_roots,
        modules: debug_modules(graph.modules),
        scopes: graph.scopes,
        contexts: graph.contexts,
        symbols: graph.symbols,
        bindings: graph.bindings,
        definitions: graph.definitions,
        diagnostics,
    })
}

pub fn analyze_debug_references(
    options: DebugReferencesOptions,
) -> Result<DebugReferencesOutput, Diagnostic> {
    let output = analyze_semantic_project(
        options.project_root,
        options.source_roots,
        options.target_python,
    )?;
    let SemanticDebugData {
        target_python,
        project_root,
        source_roots,
        graph,
        diagnostics,
    } = output;

    Ok(DebugReferencesOutput {
        schema_version: 1,
        target_python,
        project_root,
        source_roots,
        modules: debug_modules(graph.modules),
        scopes: graph.scopes,
        contexts: graph.contexts,
        symbols: graph.symbols,
        bindings: graph.bindings,
        binding_sets: graph.binding_sets,
        flow_uncertainty_sets: graph.flow_uncertainty_sets,
        definitions: graph.definitions,
        references: graph.references,
        context_flow_statuses: graph.context_flow_statuses,
        diagnostics,
    })
}

struct SemanticDebugData {
    target_python: PythonVersion,
    project_root: String,
    source_roots: Vec<SourceRootOutput>,
    graph: SemanticGraph,
    diagnostics: Vec<Diagnostic>,
}

fn analyze_semantic_project(
    project_root: PathBuf,
    source_roots: Vec<PathBuf>,
    target_python: Option<PythonVersion>,
) -> Result<SemanticDebugData, Diagnostic> {
    let project = discover_project(DiscoveryOptions {
        project_root,
        explicit_source_roots: source_roots,
        target_python,
    })
    .map_err(|error| Diagnostic::error("CULL_P0000", error.to_string()))?;

    let mut diagnostics = project.diagnostics.clone();
    let mut builder = SemanticGraphBuilder::new();

    for module in &project.modules {
        let bytes = match fs::read(&module.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                diagnostics.push(
                    Diagnostic::error("CULL_P0100", format!("failed to read source: {error}"))
                        .with_path(module.display_path.clone()),
                );
                continue;
            }
        };

        let source = match decode_python_source(&bytes) {
            Ok(source) => source,
            Err(error) => {
                diagnostics.push(
                    Diagnostic::error("CULL_P0101", error.to_string())
                        .with_path(module.display_path.clone()),
                );
                continue;
            }
        };

        let input = ParseInput {
            file_id: module.file,
            module_id: module.id,
            module_name: &module.name,
            display_path: &module.display_path,
            source: &source,
            target_python: project.target_python,
        };

        match parse_ruff_module(input) {
            Ok(parsed) => {
                collect_module_semantics(&mut builder, &mut diagnostics, input, &parsed);
                analyze_module_flow(&mut builder, input.module_id, &parsed);
            }
            Err(mut parse_diagnostics) => diagnostics.append(&mut parse_diagnostics),
        }
    }

    diagnostics.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then(
                left.range
                    .as_ref()
                    .map(|range| range.start)
                    .cmp(&right.range.as_ref().map(|range| range.start)),
            )
            .then(left.code.cmp(&right.code))
            .then(left.message.cmp(&right.message))
    });

    let graph = builder.finish();
    Ok(SemanticDebugData {
        target_python: project.target_python,
        project_root: crate::paths::slash_path(&project.project_root),
        source_roots: project.source_root_output(),
        graph,
        diagnostics,
    })
}

fn debug_modules(modules: Vec<cull_core::SemanticModule>) -> Vec<DebugBindingModule> {
    modules
        .into_iter()
        .map(|module| DebugBindingModule {
            id: module.id,
            file: module.file,
            name: module.name,
            path: module.path,
            future_annotations: module.future_annotations,
            scope: module.scope,
            context: module.context,
        })
        .collect()
}
