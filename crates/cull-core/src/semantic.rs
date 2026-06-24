use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    BindingId, ContextId, DefId, DefinitionKind, FileId, ModuleId, ScopeId, SymbolId, TextRange,
};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticGraph {
    pub modules: Vec<SemanticModule>,
    pub scopes: Vec<ScopeFact>,
    pub contexts: Vec<ContextFact>,
    pub symbols: Vec<SymbolFact>,
    pub bindings: Vec<BindingFact>,
    pub definitions: Vec<SemanticDefinition>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticModule {
    pub id: ModuleId,
    pub file: FileId,
    pub name: String,
    pub path: String,
    pub future_annotations: bool,
    pub scope: ScopeId,
    pub context: ContextId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScopeFact {
    pub id: ScopeId,
    pub module: ModuleId,
    pub kind: ScopeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ScopeId>,
    pub context: ContextId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_definition: Option<DefId>,
    pub name: String,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Module,
    Function,
    Class,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextFact {
    pub id: ContextId,
    pub module: ModuleId,
    pub kind: ContextKind,
    pub scope: ScopeId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<ContextId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_definition: Option<DefId>,
    pub range: TextRange,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    ModuleBody,
    FunctionBody,
    ClassBody,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SymbolFact {
    pub id: SymbolId,
    pub module: ModuleId,
    pub scope: ScopeId,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BindingFact {
    pub id: BindingId,
    pub module: ModuleId,
    pub scope: ScopeId,
    pub symbol: SymbolId,
    pub kind: BindingKind,
    pub name: String,
    pub order: u32,
    pub range: TextRange,
    pub name_range: TextRange,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<DefId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replaces: Option<BindingId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingKind {
    Parameter,
    FunctionDefinition,
    ClassDefinition,
    Assignment,
    AnnotatedAssignment,
    AugmentedAssignment,
    Import,
    ImportFrom,
    TypeAlias,
    Delete,
    ForTarget,
    WithTarget,
    ExceptTarget,
    MatchCapture,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticDefinition {
    pub id: DefId,
    pub module: ModuleId,
    pub binding: BindingId,
    pub scope: ScopeId,
    pub context: ContextId,
    pub kind: DefinitionKind,
    pub name: String,
    pub qualified_name: String,
    pub range: TextRange,
    pub name_range: TextRange,
    pub reportable: bool,
    pub is_async: bool,
}

#[derive(Debug, Default)]
pub struct SemanticGraphBuilder {
    graph: SemanticGraph,
    symbols_by_scope_name: BTreeMap<(ScopeId, String), SymbolId>,
    last_binding_by_symbol: BTreeMap<SymbolId, BindingId>,
    next_binding_order_by_module: BTreeMap<ModuleId, u32>,
}

impl SemanticGraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> SemanticGraph {
        self.graph
    }

    pub fn add_scope_with_context(&mut self, input: ScopeContextInput) -> (ScopeId, ContextId) {
        let scope = ScopeId::new(self.graph.scopes.len() as u32);
        let context = ContextId::new(self.graph.contexts.len() as u32);

        self.graph.scopes.push(ScopeFact {
            id: scope,
            module: input.module,
            kind: input.scope_kind,
            parent: input.parent_scope,
            context,
            owner_definition: input.owner_definition,
            name: input.name,
            range: input.range,
        });
        self.graph.contexts.push(ContextFact {
            id: context,
            module: input.module,
            kind: input.context_kind,
            scope,
            parent: input.parent_context,
            owner_definition: input.owner_definition,
            range: input.range,
        });

        (scope, context)
    }

    pub fn add_module(&mut self, module: SemanticModule) {
        self.graph.modules.push(module);
    }

    pub fn symbol(&mut self, module: ModuleId, scope: ScopeId, name: &str) -> SymbolId {
        let key = (scope, name.to_owned());
        if let Some(id) = self.symbols_by_scope_name.get(&key) {
            return *id;
        }

        let id = SymbolId::new(self.graph.symbols.len() as u32);
        self.graph.symbols.push(SymbolFact {
            id,
            module,
            scope,
            name: name.to_owned(),
        });
        self.symbols_by_scope_name.insert(key, id);
        id
    }

    pub fn add_binding(&mut self, input: BindingInput) -> BindingId {
        let id = BindingId::new(self.graph.bindings.len() as u32);
        let order = self
            .next_binding_order_by_module
            .entry(input.module)
            .and_modify(|next| *next += 1)
            .or_insert(1);
        let order = *order - 1;
        let replaces = self.last_binding_by_symbol.insert(input.symbol, id);

        self.graph.bindings.push(BindingFact {
            id,
            module: input.module,
            scope: input.scope,
            symbol: input.symbol,
            kind: input.kind,
            name: input.name,
            order,
            range: input.range,
            name_range: input.name_range,
            definition: None,
            replaces,
        });

        id
    }

    pub fn add_definition(&mut self, input: DefinitionInput) -> DefId {
        let id = DefId::new(self.graph.definitions.len() as u32);
        self.graph.definitions.push(SemanticDefinition {
            id,
            module: input.module,
            binding: input.binding,
            scope: input.scope,
            context: input.context,
            kind: input.kind,
            name: input.name,
            qualified_name: input.qualified_name,
            range: input.range,
            name_range: input.name_range,
            reportable: input.reportable,
            is_async: input.is_async,
        });

        self.binding_mut(input.binding).definition = Some(id);
        self.scope_mut(input.scope).owner_definition = Some(id);
        self.context_mut(input.context).owner_definition = Some(id);

        id
    }

    fn binding_mut(&mut self, id: BindingId) -> &mut BindingFact {
        &mut self.graph.bindings[id.as_u32() as usize]
    }

    fn scope_mut(&mut self, id: ScopeId) -> &mut ScopeFact {
        &mut self.graph.scopes[id.as_u32() as usize]
    }

    fn context_mut(&mut self, id: ContextId) -> &mut ContextFact {
        &mut self.graph.contexts[id.as_u32() as usize]
    }
}

#[derive(Clone, Debug)]
pub struct ScopeContextInput {
    pub module: ModuleId,
    pub scope_kind: ScopeKind,
    pub context_kind: ContextKind,
    pub parent_scope: Option<ScopeId>,
    pub parent_context: Option<ContextId>,
    pub owner_definition: Option<DefId>,
    pub name: String,
    pub range: TextRange,
}

#[derive(Clone, Debug)]
pub struct BindingInput {
    pub module: ModuleId,
    pub scope: ScopeId,
    pub symbol: SymbolId,
    pub kind: BindingKind,
    pub name: String,
    pub range: TextRange,
    pub name_range: TextRange,
}

#[derive(Clone, Debug)]
pub struct DefinitionInput {
    pub module: ModuleId,
    pub binding: BindingId,
    pub scope: ScopeId,
    pub context: ContextId,
    pub kind: DefinitionKind,
    pub name: String,
    pub qualified_name: String,
    pub range: TextRange,
    pub name_range: TextRange,
    pub reportable: bool,
    pub is_async: bool,
}
