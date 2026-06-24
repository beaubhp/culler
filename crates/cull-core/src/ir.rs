use serde::{Deserialize, Serialize};

use crate::{DefId, FileId, ModuleId, TextRange};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModuleIr {
    pub id: ModuleId,
    pub file: FileId,
    pub name: String,
    pub path: String,
    pub future_annotations: bool,
    pub definitions: Vec<DefinitionIr>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinitionIr {
    pub id: DefId,
    pub key: DefinitionKey,
    pub kind: DefinitionKind,
    pub name: String,
    pub range: TextRange,
    pub name_range: TextRange,
    pub is_async: bool,
    pub decorator_count: usize,
    pub type_parameter_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DefinitionKey {
    pub module: String,
    pub kind: DefinitionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lexical_parent: Option<String>,
    pub name: String,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DefinitionKind {
    Function,
    Class,
}
