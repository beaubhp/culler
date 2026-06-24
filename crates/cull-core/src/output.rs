use serde::{Deserialize, Serialize};

use crate::{
    BindingFact, BindingSetFact, ContextFact, ContextFlowStatusFact, DefinitionKind, Diagnostic,
    FlowUncertaintySetFact, PythonVersion, ReferenceFact, ScopeFact, SemanticDefinition,
    SymbolFact, TextRange,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugDefinitionsOutput {
    pub schema_version: u32,
    pub target_python: PythonVersion,
    pub project_root: String,
    pub source_roots: Vec<SourceRootOutput>,
    pub modules: Vec<DebugModule>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceRootOutput {
    pub path: String,
    pub kind: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugBindingsOutput {
    pub schema_version: u32,
    pub target_python: PythonVersion,
    pub project_root: String,
    pub source_roots: Vec<SourceRootOutput>,
    pub modules: Vec<DebugBindingModule>,
    pub scopes: Vec<ScopeFact>,
    pub contexts: Vec<ContextFact>,
    pub symbols: Vec<SymbolFact>,
    pub bindings: Vec<BindingFact>,
    pub definitions: Vec<SemanticDefinition>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugBindingModule {
    pub id: crate::ModuleId,
    pub file: crate::FileId,
    pub name: String,
    pub path: String,
    pub future_annotations: bool,
    pub scope: crate::ScopeId,
    pub context: crate::ContextId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugReferencesOutput {
    pub schema_version: u32,
    pub target_python: PythonVersion,
    pub project_root: String,
    pub source_roots: Vec<SourceRootOutput>,
    pub modules: Vec<DebugBindingModule>,
    pub scopes: Vec<ScopeFact>,
    pub contexts: Vec<ContextFact>,
    pub symbols: Vec<SymbolFact>,
    pub bindings: Vec<BindingFact>,
    pub binding_sets: Vec<BindingSetFact>,
    pub flow_uncertainty_sets: Vec<FlowUncertaintySetFact>,
    pub definitions: Vec<SemanticDefinition>,
    pub references: Vec<ReferenceFact>,
    pub context_flow_statuses: Vec<ContextFlowStatusFact>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugModule {
    pub name: String,
    pub path: String,
    pub future_annotations: bool,
    pub definitions: Vec<DebugDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugDefinition {
    pub kind: DefinitionKind,
    pub name: String,
    pub range: TextRange,
    pub name_range: TextRange,
    pub is_async: bool,
    pub decorator_count: usize,
    pub type_parameter_count: usize,
}
