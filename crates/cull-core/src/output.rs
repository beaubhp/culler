use serde::{Deserialize, Serialize};

use crate::{DefinitionKind, Diagnostic, PythonVersion, TextRange};

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
