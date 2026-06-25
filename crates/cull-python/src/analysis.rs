use std::{fs, path::PathBuf};

use cull_core::{
    CheckOutput, DebugBindingModule, DebugBindingsOutput, DebugCandidatesOutput, DebugDefinition,
    DebugDefinitionsOutput, DebugModule, DebugReferencesOutput, Diagnostic, ProjectCompleteness,
    ProjectMode, PythonVersion, SemanticGraph, SemanticGraphBuilder, SkippedFile,
};
use ruff_python_ast::ModModule;

use crate::{
    check::{analyze_project, candidates_from_check},
    decode_python_source,
    definition_effects::finalize_definition_effects,
    discovery::{discover_project, DiscoveredModule, DiscoveredProject, DiscoveryOptions},
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

#[derive(Clone, Debug)]
pub struct CheckOptions {
    pub project_root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub target_python: Option<PythonVersion>,
    pub mode: Option<ProjectMode>,
    pub allow_partial: bool,
}

#[derive(Clone, Debug)]
pub struct DebugCandidatesOptions {
    pub project_root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub target_python: Option<PythonVersion>,
    pub mode: Option<ProjectMode>,
    pub allow_partial: bool,
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
    let SemanticProjectData {
        project,
        graph,
        diagnostics,
        ..
    } = output;

    Ok(DebugBindingsOutput {
        schema_version: 1,
        target_python: project.target_python,
        project_root: crate::paths::slash_path(&project.project_root),
        source_roots: project.source_root_output(),
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
    let SemanticProjectData {
        project,
        graph,
        diagnostics,
        ..
    } = output;

    Ok(DebugReferencesOutput {
        schema_version: 1,
        target_python: project.target_python,
        project_root: crate::paths::slash_path(&project.project_root),
        source_roots: project.source_root_output(),
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
        definition_effect_sets: graph.definition_effect_sets,
        overload_groups: graph.overload_groups,
        internal_candidates: graph.internal_candidates,
        diagnostics,
    })
}

pub(crate) struct SemanticProjectData {
    pub(crate) project: DiscoveredProject,
    pub(crate) parsed_modules: Vec<ParsedProjectModule>,
    pub(crate) graph: SemanticGraph,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) project_completeness: ProjectCompleteness,
}

pub(crate) struct ParsedProjectModule {
    pub(crate) module: DiscoveredModule,
    pub(crate) source_text: String,
    pub(crate) syntax: ModModule,
}

pub fn analyze_check(options: CheckOptions) -> Result<CheckOutput, Diagnostic> {
    let mode_override = options.mode;
    let allow_partial_override = options.allow_partial;
    let data = analyze_semantic_project(
        options.project_root,
        options.source_roots,
        options.target_python,
    )?;
    Ok(analyze_project(data, mode_override, allow_partial_override))
}

pub fn analyze_debug_candidates(
    options: DebugCandidatesOptions,
) -> Result<DebugCandidatesOutput, Diagnostic> {
    let mode_override = options.mode;
    let allow_partial_override = options.allow_partial;
    let data = analyze_semantic_project(
        options.project_root,
        options.source_roots,
        options.target_python,
    )?;
    let output = analyze_project(data, mode_override, allow_partial_override);
    let candidates = candidates_from_check(&output);
    Ok(DebugCandidatesOutput {
        schema_version: output.schema_version,
        analysis: output.analysis.clone(),
        project_root: output.project_root,
        source_roots: output.source_roots,
        project_completeness: output.project_completeness,
        candidates,
        diagnostics: output.diagnostics,
    })
}

fn analyze_semantic_project(
    project_root: PathBuf,
    source_roots: Vec<PathBuf>,
    target_python: Option<PythonVersion>,
) -> Result<SemanticProjectData, Diagnostic> {
    let project = discover_project(DiscoveryOptions {
        project_root,
        explicit_source_roots: source_roots,
        target_python,
    })
    .map_err(|error| Diagnostic::error("CULL_P0000", error.to_string()))?;

    let mut diagnostics = project.diagnostics.clone();
    let mut builder = SemanticGraphBuilder::new();
    let mut parsed_modules = Vec::new();
    let mut skipped_files = Vec::new();

    for module in &project.modules {
        let bytes = match fs::read(&module.path) {
            Ok(bytes) => bytes,
            Err(error) => {
                skipped_files.push(SkippedFile {
                    path: module.display_path.clone(),
                    reason: "failed to read source".to_owned(),
                    diagnostic_code: "CULL_P0100".to_owned(),
                });
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
                skipped_files.push(SkippedFile {
                    path: module.display_path.clone(),
                    reason: "failed to decode source".to_owned(),
                    diagnostic_code: "CULL_P0101".to_owned(),
                });
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
                collect_module_semantics(
                    &mut builder,
                    &mut diagnostics,
                    input,
                    &parsed,
                    module.origin_domain,
                    module.origin_evidence,
                );
                analyze_module_flow(&mut builder, input.module_id, &parsed, &input.source.text);
                parsed_modules.push(ParsedProjectModule {
                    module: module.clone(),
                    source_text: input.source.text.clone(),
                    syntax: parsed,
                });
            }
            Err(mut parse_diagnostics) => {
                skipped_files.push(SkippedFile {
                    path: module.display_path.clone(),
                    reason: "failed to parse source".to_owned(),
                    diagnostic_code: "CULL_P0201".to_owned(),
                });
                diagnostics.append(&mut parse_diagnostics);
            }
        }
    }

    finalize_definition_effects(&mut builder, &mut diagnostics);

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
    let project_completeness = if skipped_files.is_empty() {
        ProjectCompleteness::complete()
    } else {
        ProjectCompleteness::partial(skipped_files)
    };
    Ok(SemanticProjectData {
        project,
        parsed_modules,
        graph,
        diagnostics,
        project_completeness,
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
            origin_domain: module.origin_domain,
            origin_evidence: module.origin_evidence,
            scope: module.scope,
            context: module.context,
        })
        .collect()
}
