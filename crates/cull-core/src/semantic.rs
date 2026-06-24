use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    BindingId, BindingSetId, ContextId, DefId, DefinitionKind, FileId, FlowUncertaintySetId,
    LoopId, ModuleId, ReferenceId, ScopeId, SymbolId, TextRange,
};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SemanticGraph {
    pub modules: Vec<SemanticModule>,
    pub scopes: Vec<ScopeFact>,
    pub contexts: Vec<ContextFact>,
    pub symbols: Vec<SymbolFact>,
    pub bindings: Vec<BindingFact>,
    pub binding_sets: Vec<BindingSetFact>,
    pub flow_uncertainty_sets: Vec<FlowUncertaintySetFact>,
    pub definitions: Vec<SemanticDefinition>,
    pub references: Vec<ReferenceFact>,
    pub context_flow_statuses: Vec<ContextFlowStatusFact>,
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Module,
    Function,
    Class,
    Lambda,
    Comprehension,
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
    LambdaBody,
    ComprehensionBody,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BindingSetFact {
    pub id: BindingSetId,
    pub bindings: Vec<BindingId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
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
    NamedExpression,
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceFact {
    pub id: ReferenceId,
    pub module: ModuleId,
    pub source_scope: ScopeId,
    pub source_context: ContextId,
    pub source_spelling: String,
    pub semantic_name: String,
    pub lexical_target: Resolution<SymbolId>,
    pub lookup: LookupSemantics,
    pub binding_state: ReferenceBindingState,
    pub phase: ReferencePhase,
    pub role: ReferenceRole,
    pub origin_domain: OriginDomain,
    pub span: TextRange,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum Resolution<T> {
    Resolved(T),
    Ambiguous(Vec<T>),
    External,
    Unresolved(UnresolvedReason),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum UnresolvedReason {
    UnsupportedSyntax,
    InvalidGlobalDeclaration,
    InvalidNonlocalDeclaration,
    MissingNonlocalBinding,
    ConflictingDeclaration,
    DeferredAnnotation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum LookupSemantics {
    Direct,
    GlobalThenBuiltin {
        global_symbol: SymbolId,
    },
    ClassLocalThenGlobalThenBuiltin {
        class_symbol: SymbolId,
        global_symbol: SymbolId,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "value")]
pub enum ReferenceBindingState {
    NotApplicable,
    Analyzed(BindingState),
    NotAnalyzed(FlowFailureReason),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BindingState {
    pub reachability: LocalReachability,
    pub bindings: BindingSetId,
    pub residual: ResidualLookup,
    pub uncertainty: FlowUncertaintySetId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalReachability {
    MayExecute,
    Unreachable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidualLookup {
    None,
    UnboundLocal,
    RuntimeGlobalThenBuiltin,
    RuntimeFreeVariable,
    BuiltinOrNameError,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FlowUncertaintySetFact {
    pub id: FlowUncertaintySetId,
    pub uncertainties: Vec<FlowUncertaintyKind>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowUncertaintyKind {
    OpaqueCallMayMutateGlobal,
    OpaqueCallMayMutateClosure,
    DynamicNamespaceMutation,
    SuspensionPoint,
    ComplexExceptionFlow,
    FailedPartialMatch,
    UnsupportedFlow,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextFlowStatusFact {
    pub context: ContextId,
    pub status: ContextFlowStatus,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "reason")]
pub enum ContextFlowStatus {
    Complete,
    Unsupported(FlowFailureReason),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum FlowFailureReason {
    ResourceBudgetExceeded(FlowResourceBudget),
    UnsupportedFlow,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowResourceBudget {
    BlockCount,
    WorklistIterations,
    StoredFlowFacts,
    BindingSetMemory,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "loop_id")]
pub enum CompletionKind {
    Normal,
    Return,
    Raise,
    Break(LoopId),
    Continue(LoopId),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferencePhase {
    DefinitionTime,
    BodyRuntime,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRole {
    Value,
    Decorator,
    DefaultValue,
    BaseClass,
    ClassKeyword,
    ComprehensionIterable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginDomain {
    Production,
    Test,
    Unknown,
}

#[derive(Debug, Default)]
pub struct SemanticGraphBuilder {
    graph: SemanticGraph,
    symbols_by_scope_name: BTreeMap<(ScopeId, String), SymbolId>,
    binding_sets_by_bindings: BTreeMap<Vec<BindingId>, BindingSetId>,
    flow_uncertainty_sets_by_kinds: BTreeMap<Vec<FlowUncertaintyKind>, FlowUncertaintySetId>,
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

    pub fn graph(&self) -> &SemanticGraph {
        &self.graph
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
        self.graph
            .context_flow_statuses
            .push(ContextFlowStatusFact {
                context,
                status: ContextFlowStatus::Complete,
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

    pub fn add_reference(&mut self, input: ReferenceInput) -> ReferenceId {
        let id = ReferenceId::new(self.graph.references.len() as u32);
        self.graph.references.push(ReferenceFact {
            id,
            module: input.module,
            source_scope: input.source_scope,
            source_context: input.source_context,
            source_spelling: input.source_spelling,
            semantic_name: input.semantic_name,
            lexical_target: input.lexical_target,
            lookup: input.lookup,
            binding_state: input.binding_state,
            phase: input.phase,
            role: input.role,
            origin_domain: input.origin_domain,
            span: input.span,
        });
        id
    }

    pub fn intern_binding_set<I>(&mut self, bindings: I) -> BindingSetId
    where
        I: IntoIterator<Item = BindingId>,
    {
        let mut bindings = bindings.into_iter().collect::<Vec<_>>();
        bindings.sort();
        bindings.dedup();

        if let Some(id) = self.binding_sets_by_bindings.get(&bindings) {
            return *id;
        }

        let id = BindingSetId::new(self.graph.binding_sets.len() as u32);
        self.graph.binding_sets.push(BindingSetFact {
            id,
            bindings: bindings.clone(),
        });
        self.binding_sets_by_bindings.insert(bindings, id);
        id
    }

    pub fn intern_flow_uncertainty_set<I>(&mut self, uncertainties: I) -> FlowUncertaintySetId
    where
        I: IntoIterator<Item = FlowUncertaintyKind>,
    {
        let mut uncertainties = uncertainties.into_iter().collect::<Vec<_>>();
        uncertainties.sort();
        uncertainties.dedup();

        if let Some(id) = self.flow_uncertainty_sets_by_kinds.get(&uncertainties) {
            return *id;
        }

        let id = FlowUncertaintySetId::new(self.graph.flow_uncertainty_sets.len() as u32);
        self.graph
            .flow_uncertainty_sets
            .push(FlowUncertaintySetFact {
                id,
                uncertainties: uncertainties.clone(),
            });
        self.flow_uncertainty_sets_by_kinds
            .insert(uncertainties, id);
        id
    }

    pub fn set_reference_binding_state(
        &mut self,
        reference: ReferenceId,
        binding_state: ReferenceBindingState,
    ) {
        self.reference_mut(reference).binding_state = binding_state;
    }

    pub fn set_context_flow_status(&mut self, context: ContextId, status: ContextFlowStatus) {
        if let Some(fact) = self
            .graph
            .context_flow_statuses
            .iter_mut()
            .find(|fact| fact.context == context)
        {
            fact.status = status;
        }
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

    fn reference_mut(&mut self, id: ReferenceId) -> &mut ReferenceFact {
        &mut self.graph.references[id.as_u32() as usize]
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

#[derive(Clone, Debug)]
pub struct ReferenceInput {
    pub module: ModuleId,
    pub source_scope: ScopeId,
    pub source_context: ContextId,
    pub source_spelling: String,
    pub semantic_name: String,
    pub lexical_target: Resolution<SymbolId>,
    pub lookup: LookupSemantics,
    pub binding_state: ReferenceBindingState,
    pub phase: ReferencePhase,
    pub role: ReferenceRole,
    pub origin_domain: OriginDomain,
    pub span: TextRange,
}
