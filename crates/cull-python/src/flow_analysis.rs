use std::collections::{BTreeMap, BTreeSet};

use cull_core::{
    BindingId, BindingKind, BindingState, ContextFlowStatus, ContextId, DefinitionKind,
    FlowFailureReason, FlowUncertaintyKind, LocalReachability, LookupSemantics, ModuleId,
    ReferenceBindingState, ReferenceId, ResidualLookup, Resolution, ScopeId, ScopeKind,
    SemanticGraph, SemanticGraphBuilder, SymbolId, TextRange,
};
use ruff_python_ast::{
    Expr, ExprContext, FStringPart, InterpolatedStringElement, ModModule, Pattern, Stmt,
    StmtClassDef, StmtFunctionDef,
};

use crate::ruff_frontend::to_range;

pub(crate) fn analyze_module_flow(
    builder: &mut SemanticGraphBuilder,
    module: ModuleId,
    syntax: &ModModule,
) {
    let facts = FlowFacts::new(builder.graph(), module);
    let Some(module_context) = facts.module_context else {
        return;
    };

    let mut analyzer = FlowAnalyzer {
        builder,
        facts,
        unsupported_contexts: BTreeMap::new(),
    };
    let entry = analyzer.module_entry_env();
    let _ = analyzer.analyze_block(&syntax.body, module_context, entry);

    analyzer.finalize_context_statuses();
    analyzer.mark_unvisited_references_not_analyzed(module);
}

#[derive(Clone)]
struct FlowFacts {
    module_context: Option<ContextId>,
    module_scope: Option<ScopeId>,
    contexts: BTreeMap<ContextId, ContextInfo>,
    scopes: BTreeMap<ScopeId, ScopeInfo>,
    symbol_scopes: BTreeMap<SymbolId, ScopeId>,
    symbols_by_scope: BTreeMap<ScopeId, BTreeSet<SymbolId>>,
    bindings: BTreeMap<BindingId, BindingInfo>,
    bindings_by_range_kind: BTreeMap<RangeKindKey, Vec<BindingId>>,
    references: BTreeMap<ReferenceId, ReferenceInfo>,
    references_by_context_range_name: BTreeMap<ReferenceKey, ReferenceId>,
    definitions_by_name_range_kind: BTreeMap<DefinitionKey, DefinitionInfo>,
    anonymous_contexts_by_range_kind: BTreeMap<AnonymousContextKey, ContextInfo>,
    parameter_bindings_by_scope: BTreeMap<ScopeId, Vec<BindingId>>,
}

#[derive(Clone, Copy)]
struct ContextInfo {
    context: ContextId,
    scope: ScopeId,
    kind: cull_core::ContextKind,
}

#[derive(Clone, Copy)]
struct ScopeInfo {
    kind: ScopeKind,
}

#[derive(Clone)]
struct BindingInfo {
    symbol: SymbolId,
    kind: BindingKind,
}

#[derive(Clone)]
struct ReferenceInfo {
    lexical_target: Resolution<SymbolId>,
    lookup: LookupSemantics,
}

#[derive(Clone, Copy)]
struct DefinitionInfo {
    scope: ScopeId,
    context: ContextId,
    binding: BindingId,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct RangeKindKey {
    start: u32,
    end: u32,
    kind: BindingKind,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct ReferenceKey {
    context: ContextId,
    start: u32,
    end: u32,
    source_spelling: String,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct DefinitionKey {
    start: u32,
    end: u32,
    kind: DefinitionKind,
}

#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
struct AnonymousContextKey {
    start: u32,
    end: u32,
    kind: ScopeKind,
}

impl FlowFacts {
    fn new(graph: &SemanticGraph, module: ModuleId) -> Self {
        let mut contexts = BTreeMap::new();
        let mut scopes = BTreeMap::new();
        let mut module_context = None;
        let mut module_scope = None;
        for scope in graph.scopes.iter().filter(|scope| scope.module == module) {
            if scope.kind == ScopeKind::Module {
                module_scope = Some(scope.id);
            }
            scopes.insert(scope.id, ScopeInfo { kind: scope.kind });
        }
        for context in graph
            .contexts
            .iter()
            .filter(|context| context.module == module)
        {
            if matches!(context.kind, cull_core::ContextKind::ModuleBody) {
                module_context = Some(context.id);
            }
            contexts.insert(
                context.id,
                ContextInfo {
                    context: context.id,
                    scope: context.scope,
                    kind: context.kind,
                },
            );
        }

        let mut symbol_scopes = BTreeMap::new();
        let mut symbols_by_scope: BTreeMap<ScopeId, BTreeSet<SymbolId>> = BTreeMap::new();
        for symbol in graph
            .symbols
            .iter()
            .filter(|symbol| symbol.module == module)
        {
            symbol_scopes.insert(symbol.id, symbol.scope);
            symbols_by_scope
                .entry(symbol.scope)
                .or_default()
                .insert(symbol.id);
        }

        let mut bindings = BTreeMap::new();
        let mut bindings_by_range_kind: BTreeMap<RangeKindKey, Vec<BindingId>> = BTreeMap::new();
        let mut parameter_bindings_by_scope: BTreeMap<ScopeId, Vec<BindingId>> = BTreeMap::new();
        for binding in graph
            .bindings
            .iter()
            .filter(|binding| binding.module == module)
        {
            bindings.insert(
                binding.id,
                BindingInfo {
                    symbol: binding.symbol,
                    kind: binding.kind,
                },
            );
            bindings_by_range_kind
                .entry(RangeKindKey::new(binding.name_range, binding.kind))
                .or_default()
                .push(binding.id);
            if binding.kind == BindingKind::Parameter {
                parameter_bindings_by_scope
                    .entry(binding.scope)
                    .or_default()
                    .push(binding.id);
            }
        }
        for binding_ids in bindings_by_range_kind.values_mut() {
            binding_ids.sort();
        }
        for binding_ids in parameter_bindings_by_scope.values_mut() {
            binding_ids.sort();
        }

        let mut references = BTreeMap::new();
        let mut references_by_context_range_name = BTreeMap::new();
        for reference in graph
            .references
            .iter()
            .filter(|reference| reference.module == module)
        {
            references.insert(
                reference.id,
                ReferenceInfo {
                    lexical_target: reference.lexical_target.clone(),
                    lookup: reference.lookup.clone(),
                },
            );
            references_by_context_range_name.insert(
                ReferenceKey {
                    context: reference.source_context,
                    start: reference.span.start,
                    end: reference.span.end,
                    source_spelling: reference.source_spelling.clone(),
                },
                reference.id,
            );
        }

        let mut definitions_by_name_range_kind = BTreeMap::new();
        for definition in graph
            .definitions
            .iter()
            .filter(|definition| definition.module == module)
        {
            definitions_by_name_range_kind.insert(
                DefinitionKey {
                    start: definition.name_range.start,
                    end: definition.name_range.end,
                    kind: definition.kind,
                },
                DefinitionInfo {
                    scope: definition.scope,
                    context: definition.context,
                    binding: definition.binding,
                },
            );
        }

        let mut anonymous_contexts_by_range_kind = BTreeMap::new();
        for scope in graph
            .scopes
            .iter()
            .filter(|scope| scope.module == module)
            .filter(|scope| matches!(scope.kind, ScopeKind::Lambda | ScopeKind::Comprehension))
        {
            anonymous_contexts_by_range_kind.insert(
                AnonymousContextKey {
                    start: scope.range.start,
                    end: scope.range.end,
                    kind: scope.kind,
                },
                ContextInfo {
                    context: scope.context,
                    scope: scope.id,
                    kind: graph.contexts[scope.context.as_u32() as usize].kind,
                },
            );
        }

        Self {
            module_context,
            module_scope,
            contexts,
            scopes,
            symbol_scopes,
            symbols_by_scope,
            bindings,
            bindings_by_range_kind,
            references,
            references_by_context_range_name,
            definitions_by_name_range_kind,
            anonymous_contexts_by_range_kind,
            parameter_bindings_by_scope,
        }
    }

    fn context_scope(&self, context: ContextId) -> Option<ScopeId> {
        self.contexts.get(&context).map(|context| context.scope)
    }

    fn scope_kind(&self, scope: ScopeId) -> Option<ScopeKind> {
        self.scopes.get(&scope).map(|scope| scope.kind)
    }

    fn binding(&self, id: BindingId) -> Option<&BindingInfo> {
        self.bindings.get(&id)
    }

    fn binding_ids(&self, range: TextRange, kind: BindingKind) -> Vec<BindingId> {
        self.bindings_by_range_kind
            .get(&RangeKindKey::new(range, kind))
            .cloned()
            .unwrap_or_default()
    }

    fn definition(&self, name_range: TextRange, kind: DefinitionKind) -> Option<DefinitionInfo> {
        self.definitions_by_name_range_kind
            .get(&DefinitionKey {
                start: name_range.start,
                end: name_range.end,
                kind,
            })
            .copied()
    }

    fn anonymous_context(&self, range: TextRange, kind: ScopeKind) -> Option<ContextInfo> {
        self.anonymous_contexts_by_range_kind
            .get(&AnonymousContextKey {
                start: range.start,
                end: range.end,
                kind,
            })
            .copied()
    }

    fn reference(
        &self,
        context: ContextId,
        range: TextRange,
        source_spelling: &str,
    ) -> Option<ReferenceId> {
        self.references_by_context_range_name
            .get(&ReferenceKey {
                context,
                start: range.start,
                end: range.end,
                source_spelling: source_spelling.to_owned(),
            })
            .copied()
    }
}

impl RangeKindKey {
    fn new(range: TextRange, kind: BindingKind) -> Self {
        Self {
            start: range.start,
            end: range.end,
            kind,
        }
    }
}

struct FlowAnalyzer<'a> {
    builder: &'a mut SemanticGraphBuilder,
    facts: FlowFacts,
    unsupported_contexts: BTreeMap<ContextId, FlowFailureReason>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FlowEnv {
    reachable: bool,
    slots: BTreeMap<SymbolId, SlotState>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SlotState {
    bindings: BTreeSet<BindingId>,
    residuals: BTreeSet<SlotResidual>,
    uncertainties: BTreeSet<FlowUncertaintyKind>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum SlotResidual {
    Unbound,
    RuntimeGlobalThenBuiltin,
    RuntimeFreeVariable,
    BuiltinOrNameError,
}

#[derive(Clone, Debug, Default)]
struct FlowOutcome {
    normal: Option<FlowEnv>,
    return_: Option<FlowEnv>,
    raise: Option<FlowEnv>,
    break_: Option<FlowEnv>,
    continue_: Option<FlowEnv>,
}

impl FlowEnv {
    fn reachable() -> Self {
        Self {
            reachable: true,
            slots: BTreeMap::new(),
        }
    }

    fn unreachable() -> Self {
        Self {
            reachable: false,
            slots: BTreeMap::new(),
        }
    }

    fn join(&self, other: &Self) -> Self {
        if !self.reachable {
            return other.clone();
        }
        if !other.reachable {
            return self.clone();
        }

        let mut joined = self.clone();
        joined.reachable = self.reachable || other.reachable;
        for (symbol, state) in &other.slots {
            joined
                .slots
                .entry(*symbol)
                .and_modify(|existing| existing.join_assign(state))
                .or_insert_with(|| state.clone());
        }
        joined
    }

    fn get_or_default(&self, facts: &FlowFacts, context: ContextId, symbol: SymbolId) -> SlotState {
        self.slots.get(&symbol).cloned().unwrap_or_else(|| {
            let current_scope = facts.context_scope(context);
            let symbol_scope = facts.symbol_scopes.get(&symbol).copied();
            match (current_scope, symbol_scope) {
                (Some(current), Some(scope)) if current == scope => SlotState::unbound(),
                (_, Some(scope)) if facts.scope_kind(scope) == Some(ScopeKind::Module) => {
                    match facts.contexts.get(&context).map(|context| context.kind) {
                        Some(
                            cull_core::ContextKind::ModuleBody | cull_core::ContextKind::ClassBody,
                        ) => SlotState::unbound(),
                        _ => SlotState::runtime_global(),
                    }
                }
                _ => SlotState::runtime_free(),
            }
        })
    }

    fn write_binding(&mut self, facts: &FlowFacts, binding: BindingId) {
        let Some(info) = facts.binding(binding) else {
            return;
        };
        if info.kind == BindingKind::Delete {
            self.slots.insert(info.symbol, SlotState::unbound());
        } else {
            self.slots.insert(info.symbol, SlotState::known(binding));
        }
    }

    fn add_uncertainty_to_symbol(
        &mut self,
        symbol: SymbolId,
        residual: SlotResidual,
        uncertainty: FlowUncertaintyKind,
    ) {
        let state = self.slots.entry(symbol).or_insert_with(SlotState::unbound);
        state.residuals.insert(residual);
        state.uncertainties.insert(uncertainty);
    }

    fn add_uncertainty_to_all_slots(&mut self, uncertainty: FlowUncertaintyKind) {
        for state in self.slots.values_mut() {
            state.uncertainties.insert(uncertainty);
        }
    }
}

impl SlotState {
    fn known(binding: BindingId) -> Self {
        Self {
            bindings: BTreeSet::from([binding]),
            residuals: BTreeSet::new(),
            uncertainties: BTreeSet::new(),
        }
    }

    fn unbound() -> Self {
        Self {
            bindings: BTreeSet::new(),
            residuals: BTreeSet::from([SlotResidual::Unbound]),
            uncertainties: BTreeSet::new(),
        }
    }

    fn runtime_global() -> Self {
        Self {
            bindings: BTreeSet::new(),
            residuals: BTreeSet::from([SlotResidual::RuntimeGlobalThenBuiltin]),
            uncertainties: BTreeSet::new(),
        }
    }

    fn runtime_free() -> Self {
        Self {
            bindings: BTreeSet::new(),
            residuals: BTreeSet::from([SlotResidual::RuntimeFreeVariable]),
            uncertainties: BTreeSet::new(),
        }
    }

    fn join_assign(&mut self, other: &Self) {
        self.bindings.extend(other.bindings.iter().copied());
        self.residuals.extend(other.residuals.iter().copied());
        self.uncertainties
            .extend(other.uncertainties.iter().copied());
    }

    fn may_be_unbound(&self) -> bool {
        self.residuals.contains(&SlotResidual::Unbound)
    }
}

impl FlowOutcome {
    fn normal(env: FlowEnv) -> Self {
        Self {
            normal: Some(env),
            ..Self::default()
        }
    }

    fn join_assign(&mut self, other: Self) {
        join_option_env(&mut self.normal, other.normal);
        join_option_env(&mut self.return_, other.return_);
        join_option_env(&mut self.raise, other.raise);
        join_option_env(&mut self.break_, other.break_);
        join_option_env(&mut self.continue_, other.continue_);
    }

    fn all_completions(&self) -> Option<FlowEnv> {
        let mut joined = self.normal.clone();
        join_option_env(&mut joined, self.return_.clone());
        join_option_env(&mut joined, self.raise.clone());
        join_option_env(&mut joined, self.break_.clone());
        join_option_env(&mut joined, self.continue_.clone());
        joined
    }
}

fn join_option_env(left: &mut Option<FlowEnv>, right: Option<FlowEnv>) {
    match (left.as_mut(), right) {
        (Some(left), Some(right)) => *left = left.join(&right),
        (None, Some(right)) => *left = Some(right),
        _ => {}
    }
}

impl FlowAnalyzer<'_> {
    fn analyze_block(
        &mut self,
        statements: &[Stmt],
        context: ContextId,
        entry: FlowEnv,
    ) -> FlowOutcome {
        let mut normal = Some(entry);
        let mut abrupt = FlowOutcome::default();
        let mut unreachable_env = FlowEnv::unreachable();

        for statement in statements {
            let Some(env) = normal.take() else {
                let _ = self.analyze_statement(statement, context, unreachable_env.clone());
                continue;
            };

            let outcome = self.analyze_statement(statement, context, env);
            if let Some(env) = outcome.normal.clone() {
                unreachable_env = env.clone();
                normal = Some(env);
            } else {
                unreachable_env = FlowEnv::unreachable();
            }
            join_option_env(&mut abrupt.return_, outcome.return_);
            join_option_env(&mut abrupt.raise, outcome.raise);
            join_option_env(&mut abrupt.break_, outcome.break_);
            join_option_env(&mut abrupt.continue_, outcome.continue_);
        }

        abrupt.normal = normal;
        abrupt
    }

    fn analyze_statement(
        &mut self,
        statement: &Stmt,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        match statement {
            Stmt::FunctionDef(function) => self.analyze_function_def(function, context, env),
            Stmt::ClassDef(class) => self.analyze_class_def(class, context, env),
            Stmt::Return(return_stmt) => {
                let env = if let Some(value) = &return_stmt.value {
                    self.analyze_expr(value, context, env)
                } else {
                    env
                };
                FlowOutcome {
                    return_: Some(env),
                    ..FlowOutcome::default()
                }
            }
            Stmt::Raise(raise) => {
                let mut env = env;
                if let Some(exc) = &raise.exc {
                    env = self.analyze_expr(exc, context, env);
                }
                if let Some(cause) = &raise.cause {
                    env = self.analyze_expr(cause, context, env);
                }
                FlowOutcome {
                    raise: Some(env),
                    ..FlowOutcome::default()
                }
            }
            Stmt::Break(_) => FlowOutcome {
                break_: Some(env),
                ..FlowOutcome::default()
            },
            Stmt::Continue(_) => FlowOutcome {
                continue_: Some(env),
                ..FlowOutcome::default()
            },
            Stmt::Delete(delete) => {
                let mut env = env;
                for target in &delete.targets {
                    env = self.write_target(target, BindingKind::Delete, env);
                }
                FlowOutcome::normal(env)
            }
            Stmt::Assign(assign) => {
                let mut env = self.analyze_expr(&assign.value, context, env);
                for target in &assign.targets {
                    env = self.write_target(target, BindingKind::Assignment, env);
                }
                FlowOutcome::normal(env)
            }
            Stmt::AnnAssign(assign) => {
                let mut env = env;
                if let Some(value) = &assign.value {
                    env = self.analyze_expr(value, context, env);
                }
                env = self.write_target(&assign.target, BindingKind::AnnotatedAssignment, env);
                FlowOutcome::normal(env)
            }
            Stmt::AugAssign(assign) => {
                let env = self.read_target(&assign.target, context, env);
                let env = self.analyze_expr(&assign.value, context, env);
                FlowOutcome::normal(self.write_target(
                    &assign.target,
                    BindingKind::AugmentedAssignment,
                    env,
                ))
            }
            Stmt::Expr(expr) => FlowOutcome::normal(self.analyze_expr(&expr.value, context, env)),
            Stmt::Assert(assert_stmt) => {
                let mut env = self.analyze_expr(&assert_stmt.test, context, env);
                if let Some(msg) = &assert_stmt.msg {
                    env = self.analyze_expr(msg, context, env);
                }
                FlowOutcome::normal(env)
            }
            Stmt::If(if_stmt) => self.analyze_if(if_stmt, context, env),
            Stmt::While(while_stmt) => self.analyze_while(while_stmt, context, env),
            Stmt::For(for_stmt) => self.analyze_for(for_stmt, context, env),
            Stmt::With(with_stmt) => {
                let mut env = env;
                for item in &with_stmt.items {
                    env = self.analyze_expr(&item.context_expr, context, env);
                    if let Some(target) = &item.optional_vars {
                        env = self.write_target(target, BindingKind::WithTarget, env);
                    }
                }
                let mut outcome = self.analyze_block(&with_stmt.body, context, env);
                if let Some(normal) = &mut outcome.normal {
                    self.apply_call_barrier(normal, context, None);
                }
                outcome
            }
            Stmt::Try(try_stmt) => self.analyze_try(try_stmt, context, env),
            Stmt::Match(match_stmt) => self.analyze_match(match_stmt, context, env),
            Stmt::Import(import) => {
                let mut env = env;
                for alias in &import.names {
                    let range = alias
                        .asname
                        .as_ref()
                        .map(|name| to_range(name.range))
                        .unwrap_or_else(|| to_range(alias.name.range));
                    env = self.write_binding_at(range, BindingKind::Import, env);
                }
                FlowOutcome::normal(env)
            }
            Stmt::ImportFrom(import) => {
                let mut env = env;
                for alias in &import.names {
                    if alias.name.id.as_str() == "*" {
                        env.add_uncertainty_to_all_slots(
                            FlowUncertaintyKind::DynamicNamespaceMutation,
                        );
                        continue;
                    }
                    let range = alias
                        .asname
                        .as_ref()
                        .map(|name| to_range(name.range))
                        .unwrap_or_else(|| to_range(alias.name.range));
                    env = self.write_binding_at(range, BindingKind::ImportFrom, env);
                }
                FlowOutcome::normal(env)
            }
            Stmt::TypeAlias(alias) => {
                FlowOutcome::normal(self.write_target(&alias.name, BindingKind::TypeAlias, env))
            }
            Stmt::Global(_) | Stmt::Nonlocal(_) | Stmt::Pass(_) | Stmt::IpyEscapeCommand(_) => {
                FlowOutcome::normal(env)
            }
        }
    }

    fn analyze_function_def(
        &mut self,
        function: &StmtFunctionDef,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let mut env = env;
        for decorator in &function.decorator_list {
            env = self.analyze_expr(&decorator.expression, context, env);
        }
        for parameter in function.parameters.iter_source_order() {
            if let Some(default) = parameter.default() {
                env = self.analyze_expr(default, context, env);
            }
        }

        let range = to_range(function.name.range);
        if let Some(definition) = self.facts.definition(range, DefinitionKind::Function) {
            self.analyze_deferred_body(definition.scope, definition.context, &function.body);
            env.write_binding(&self.facts, definition.binding);
        }
        FlowOutcome::normal(env)
    }

    fn analyze_class_def(
        &mut self,
        class: &StmtClassDef,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let mut env = env;
        for decorator in &class.decorator_list {
            env = self.analyze_expr(&decorator.expression, context, env);
        }
        if let Some(arguments) = &class.arguments {
            for base in &arguments.args {
                env = self.analyze_expr(base, context, env);
            }
            for keyword in &arguments.keywords {
                env = self.analyze_expr(&keyword.value, context, env);
            }
        }

        let name_range = to_range(class.name.range);
        if let Some(definition) = self.facts.definition(name_range, DefinitionKind::Class) {
            let class_env = self.class_entry_env(definition.scope, env.clone());
            let class_outcome = self.analyze_block(&class.body, definition.context, class_env);
            if let Some(class_normal) = class_outcome.normal {
                env = self.merge_class_side_effects(definition.scope, env, class_normal);
            }
            env.write_binding(&self.facts, definition.binding);
        }

        FlowOutcome::normal(env)
    }

    fn analyze_deferred_body(&mut self, scope: ScopeId, context: ContextId, body: &[Stmt]) {
        let mut env = self.deferred_entry_env(scope);
        for binding in self
            .facts
            .parameter_bindings_by_scope
            .get(&scope)
            .cloned()
            .unwrap_or_default()
        {
            env.write_binding(&self.facts, binding);
        }
        let _ = self.analyze_block(body, context, env);
    }

    fn analyze_lambda(
        &mut self,
        lambda: &ruff_python_ast::ExprLambda,
        outer_context: ContextId,
        mut outer_env: FlowEnv,
    ) -> FlowEnv {
        let Some(context) = self
            .facts
            .anonymous_context(to_range(lambda.range), ScopeKind::Lambda)
        else {
            return outer_env;
        };
        if let Some(parameters) = &lambda.parameters {
            for parameter in parameters.iter_source_order() {
                if let Some(default) = parameter.default() {
                    outer_env = self.analyze_expr(default, outer_context, outer_env);
                }
            }
        }

        let mut env = self.deferred_entry_env(context.scope);
        if lambda.parameters.is_some() {
            for binding in self
                .facts
                .parameter_bindings_by_scope
                .get(&context.scope)
                .cloned()
                .unwrap_or_default()
            {
                env.write_binding(&self.facts, binding);
            }
        }
        let _ = self.analyze_expr(&lambda.body, context.context, env);
        outer_env
    }

    fn analyze_if(
        &mut self,
        if_stmt: &ruff_python_ast::StmtIf,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let after_test = self.analyze_expr(&if_stmt.test, context, env);
        let mut joined = self.analyze_block(&if_stmt.body, context, after_test.clone());
        let mut fallthrough = Some(after_test);

        for clause in &if_stmt.elif_else_clauses {
            let Some(clause_entry) = fallthrough.take() else {
                let _ = self.analyze_block(&clause.body, context, FlowEnv::unreachable());
                continue;
            };
            let clause_entry = if let Some(test) = &clause.test {
                self.analyze_expr(test, context, clause_entry)
            } else {
                clause_entry
            };
            joined.join_assign(self.analyze_block(&clause.body, context, clause_entry.clone()));
            fallthrough = if clause.test.is_some() {
                Some(clause_entry)
            } else {
                None
            };
        }

        join_option_env(&mut joined.normal, fallthrough);
        joined
    }

    fn analyze_while(
        &mut self,
        while_stmt: &ruff_python_ast::StmtWhile,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let after_test = self.analyze_expr(&while_stmt.test, context, env);
        let mut loop_entry = after_test.clone();
        let mut loop_outcome = FlowOutcome::default();
        let mut converged = false;

        for _ in 0..64 {
            let body_outcome = self.analyze_block(&while_stmt.body, context, loop_entry.clone());
            let mut next_entry = after_test.clone();
            if let Some(normal) = &body_outcome.normal {
                next_entry = next_entry.join(normal);
            }
            if let Some(continue_) = &body_outcome.continue_ {
                next_entry = next_entry.join(continue_);
            }
            loop_outcome.join_assign(body_outcome);
            if next_entry == loop_entry {
                converged = true;
                break;
            }
            loop_entry = next_entry;
        }
        if !converged {
            self.mark_context_unsupported(
                context,
                FlowFailureReason::ResourceBudgetExceeded(
                    cull_core::FlowResourceBudget::WorklistIterations,
                ),
            );
        }

        let mut normal_exhaustion = Some(loop_entry);
        if !while_stmt.orelse.is_empty() {
            normal_exhaustion = self
                .analyze_block(&while_stmt.orelse, context, normal_exhaustion.unwrap())
                .normal;
        }
        FlowOutcome {
            normal: join_envs(normal_exhaustion, loop_outcome.break_),
            return_: loop_outcome.return_,
            raise: loop_outcome.raise,
            ..FlowOutcome::default()
        }
    }

    fn analyze_for(
        &mut self,
        for_stmt: &ruff_python_ast::StmtFor,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let after_iter = self.analyze_expr(&for_stmt.iter, context, env);
        let zero_iteration = after_iter.clone();
        let mut loop_entry =
            self.write_target(&for_stmt.target, BindingKind::ForTarget, after_iter);
        let mut loop_outcome = FlowOutcome::default();
        let mut converged = false;

        for _ in 0..64 {
            let body_outcome = self.analyze_block(&for_stmt.body, context, loop_entry.clone());
            let mut next_entry = loop_entry.clone();
            if let Some(normal) = &body_outcome.normal {
                next_entry = next_entry.join(normal);
            }
            if let Some(continue_) = &body_outcome.continue_ {
                next_entry = next_entry.join(continue_);
            }
            loop_outcome.join_assign(body_outcome);
            if next_entry == loop_entry {
                converged = true;
                break;
            }
            loop_entry = next_entry;
        }
        if !converged {
            self.mark_context_unsupported(
                context,
                FlowFailureReason::ResourceBudgetExceeded(
                    cull_core::FlowResourceBudget::WorklistIterations,
                ),
            );
        }

        let mut normal_exhaustion = Some(zero_iteration.join(&loop_entry));
        if !for_stmt.orelse.is_empty() {
            normal_exhaustion = self
                .analyze_block(&for_stmt.orelse, context, normal_exhaustion.unwrap())
                .normal;
        }
        FlowOutcome {
            normal: join_envs(normal_exhaustion, loop_outcome.break_),
            return_: loop_outcome.return_,
            raise: loop_outcome.raise,
            ..FlowOutcome::default()
        }
    }

    fn analyze_try(
        &mut self,
        try_stmt: &ruff_python_ast::StmtTry,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let body_outcome = self.analyze_block(&try_stmt.body, context, env.clone());
        let mut exceptional_entry = body_outcome
            .all_completions()
            .unwrap_or_else(|| env.clone());
        exceptional_entry = exceptional_entry.join(&env);
        exceptional_entry.add_uncertainty_to_all_slots(FlowUncertaintyKind::ComplexExceptionFlow);

        let mut handler_outcomes = FlowOutcome::default();
        for handler in &try_stmt.handlers {
            let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
            let mut handler_env = exceptional_entry.clone();
            if let Some(type_) = &handler.type_ {
                handler_env = self.analyze_expr(type_, context, handler_env);
            }
            let handler_symbol = handler.name.as_ref().and_then(|name| {
                let binding = self
                    .facts
                    .binding_ids(to_range(name.range), BindingKind::ExceptTarget)
                    .into_iter()
                    .next()?;
                self.facts
                    .binding(binding)
                    .map(|info| (binding, info.symbol))
            });
            if let Some((binding, _)) = handler_symbol {
                handler_env.write_binding(&self.facts, binding);
            }
            let mut outcome = self.analyze_block(&handler.body, context, handler_env);
            if let Some((_, symbol)) = handler_symbol {
                if let Some(normal) = &mut outcome.normal {
                    normal.slots.insert(symbol, SlotState::unbound());
                }
            }
            handler_outcomes.join_assign(outcome);
        }

        let mut normal = body_outcome.normal;
        if let Some(normal_env) = normal.take() {
            normal = if try_stmt.orelse.is_empty() {
                Some(normal_env)
            } else {
                self.analyze_block(&try_stmt.orelse, context, normal_env)
                    .normal
            };
        }
        let mut combined = FlowOutcome {
            normal,
            return_: body_outcome.return_,
            raise: body_outcome.raise,
            break_: body_outcome.break_,
            continue_: body_outcome.continue_,
        };
        combined.join_assign(handler_outcomes);

        if !try_stmt.finalbody.is_empty() {
            if let Some(mut final_entry) = combined.all_completions() {
                final_entry.add_uncertainty_to_all_slots(FlowUncertaintyKind::ComplexExceptionFlow);
                let final_outcome = self.analyze_block(&try_stmt.finalbody, context, final_entry);
                combined.normal = final_outcome.normal;
                combined.return_ = join_envs(combined.return_, final_outcome.return_);
                combined.raise = join_envs(combined.raise, final_outcome.raise);
                combined.break_ = join_envs(combined.break_, final_outcome.break_);
                combined.continue_ = join_envs(combined.continue_, final_outcome.continue_);
            }
        }

        combined
    }

    fn analyze_match(
        &mut self,
        match_stmt: &ruff_python_ast::StmtMatch,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowOutcome {
        let subject_env = self.analyze_expr(&match_stmt.subject, context, env);
        let mut joined = FlowOutcome::normal(subject_env.clone());
        let mut capture_symbols = BTreeSet::new();

        for case in &match_stmt.cases {
            let mut case_env = subject_env.clone();
            for binding in self.pattern_bindings(&case.pattern) {
                if let Some(info) = self.facts.binding(binding) {
                    capture_symbols.insert(info.symbol);
                }
                case_env.write_binding(&self.facts, binding);
            }
            if let Some(guard) = &case.guard {
                case_env = self.analyze_expr(guard, context, case_env);
            }
            joined.join_assign(self.analyze_block(&case.body, context, case_env));
        }

        if let Some(normal) = &mut joined.normal {
            for symbol in capture_symbols {
                normal.add_uncertainty_to_symbol(
                    symbol,
                    SlotResidual::Unbound,
                    FlowUncertaintyKind::FailedPartialMatch,
                );
            }
        }
        joined
    }

    fn analyze_expr(&mut self, expression: &Expr, context: ContextId, env: FlowEnv) -> FlowEnv {
        match expression {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                self.record_reference(context, name.id.as_str(), to_range(name.range), &env);
                env
            }
            Expr::Name(_) => env,
            Expr::BoolOp(expr) => {
                let mut values = expr.values.iter();
                let Some(first) = values.next() else {
                    return env;
                };
                let mut env = self.analyze_expr(first, context, env);
                for value in values {
                    let evaluated = self.analyze_expr(value, context, env.clone());
                    env = env.join(&evaluated);
                }
                env
            }
            Expr::Named(expr) => {
                let env = self.analyze_expr(&expr.value, context, env);
                self.write_target(&expr.target, BindingKind::NamedExpression, env)
            }
            Expr::BinOp(expr) => {
                let env = self.analyze_expr(&expr.left, context, env);
                self.analyze_expr(&expr.right, context, env)
            }
            Expr::UnaryOp(expr) => self.analyze_expr(&expr.operand, context, env),
            Expr::Lambda(lambda) => self.analyze_lambda(lambda, context, env),
            Expr::If(expr) => {
                let env = self.analyze_expr(&expr.test, context, env);
                let body = self.analyze_expr(&expr.body, context, env.clone());
                let orelse = self.analyze_expr(&expr.orelse, context, env);
                body.join(&orelse)
            }
            Expr::Dict(expr) => {
                let mut env = env;
                for item in &expr.items {
                    if let Some(key) = &item.key {
                        env = self.analyze_expr(key, context, env);
                    }
                    env = self.analyze_expr(&item.value, context, env);
                }
                env
            }
            Expr::Set(expr) => self.analyze_exprs(&expr.elts, context, env),
            Expr::ListComp(expr) => self.analyze_comprehension(
                to_range(expr.range),
                &expr.generators,
                context,
                env,
                true,
                |analyzer, context, env| analyzer.analyze_expr(&expr.elt, context, env),
            ),
            Expr::SetComp(expr) => self.analyze_comprehension(
                to_range(expr.range),
                &expr.generators,
                context,
                env,
                true,
                |analyzer, context, env| analyzer.analyze_expr(&expr.elt, context, env),
            ),
            Expr::DictComp(expr) => self.analyze_comprehension(
                to_range(expr.range),
                &expr.generators,
                context,
                env,
                true,
                |analyzer, context, env| {
                    let mut env = env;
                    if let Some(key) = &expr.key {
                        env = analyzer.analyze_expr(key, context, env);
                    }
                    analyzer.analyze_expr(&expr.value, context, env)
                },
            ),
            Expr::Generator(expr) => self.analyze_comprehension(
                to_range(expr.range),
                &expr.generators,
                context,
                env,
                false,
                |analyzer, context, env| analyzer.analyze_expr(&expr.elt, context, env),
            ),
            Expr::Await(expr) => {
                let mut env = self.analyze_expr(&expr.value, context, env);
                env.add_uncertainty_to_all_slots(FlowUncertaintyKind::SuspensionPoint);
                self.apply_call_barrier(
                    &mut env,
                    context,
                    Some(FlowUncertaintyKind::SuspensionPoint),
                );
                env
            }
            Expr::Yield(expr) => {
                let mut env = env;
                if let Some(value) = &expr.value {
                    env = self.analyze_expr(value, context, env);
                }
                env.add_uncertainty_to_all_slots(FlowUncertaintyKind::SuspensionPoint);
                self.apply_call_barrier(
                    &mut env,
                    context,
                    Some(FlowUncertaintyKind::SuspensionPoint),
                );
                env
            }
            Expr::YieldFrom(expr) => {
                let mut env = self.analyze_expr(&expr.value, context, env);
                env.add_uncertainty_to_all_slots(FlowUncertaintyKind::SuspensionPoint);
                self.apply_call_barrier(
                    &mut env,
                    context,
                    Some(FlowUncertaintyKind::SuspensionPoint),
                );
                env
            }
            Expr::Compare(expr) => {
                let mut env = self.analyze_expr(&expr.left, context, env);
                for comparator in &expr.comparators {
                    env = self.analyze_expr(comparator, context, env);
                }
                env
            }
            Expr::Call(expr) => {
                let mut env = self.analyze_expr(&expr.func, context, env);
                for arg in &expr.arguments.args {
                    env = self.analyze_expr(arg, context, env);
                }
                for keyword in &expr.arguments.keywords {
                    env = self.analyze_expr(&keyword.value, context, env);
                }
                let dynamic_namespace = matches!(
                    expr.func.as_ref(),
                    Expr::Name(name)
                        if matches!(name.id.as_str(), "exec" | "eval" | "globals" | "locals" | "vars")
                );
                if dynamic_namespace {
                    env.add_uncertainty_to_all_slots(FlowUncertaintyKind::DynamicNamespaceMutation);
                }
                self.apply_call_barrier(&mut env, context, None);
                env
            }
            Expr::Attribute(expr) => self.analyze_expr(&expr.value, context, env),
            Expr::Subscript(expr) => {
                let env = self.analyze_expr(&expr.value, context, env);
                self.analyze_expr(&expr.slice, context, env)
            }
            Expr::Starred(expr) => self.analyze_expr(&expr.value, context, env),
            Expr::List(expr) => self.analyze_exprs(&expr.elts, context, env),
            Expr::Tuple(expr) => self.analyze_exprs(&expr.elts, context, env),
            Expr::Slice(expr) => {
                let mut env = env;
                if let Some(lower) = &expr.lower {
                    env = self.analyze_expr(lower, context, env);
                }
                if let Some(upper) = &expr.upper {
                    env = self.analyze_expr(upper, context, env);
                }
                if let Some(step) = &expr.step {
                    env = self.analyze_expr(step, context, env);
                }
                env
            }
            Expr::FString(expr) => {
                self.analyze_interpolated_string(expr.value.as_slice(), context, env)
            }
            Expr::TString(expr) => {
                let mut env = env;
                for string in expr.value.as_slice() {
                    env = self.analyze_interpolated_elements(&string.elements, context, env);
                }
                env
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::IpyEscapeCommand(_) => env,
        }
    }

    fn analyze_exprs<'a, I>(&mut self, expressions: I, context: ContextId, env: FlowEnv) -> FlowEnv
    where
        I: IntoIterator<Item = &'a Expr>,
    {
        expressions.into_iter().fold(env, |env, expression| {
            self.analyze_expr(expression, context, env)
        })
    }

    fn analyze_comprehension<F>(
        &mut self,
        range: TextRange,
        generators: &[ruff_python_ast::Comprehension],
        outer_context: ContextId,
        env: FlowEnv,
        eager: bool,
        mut analyze_result: F,
    ) -> FlowEnv
    where
        F: FnMut(&mut Self, ContextId, FlowEnv) -> FlowEnv,
    {
        let Some(first) = generators.first() else {
            return env;
        };
        let outer_after_iter = self.analyze_expr(&first.iter, outer_context, env);
        let Some(comp_context) = self
            .facts
            .anonymous_context(range, ScopeKind::Comprehension)
        else {
            return outer_after_iter;
        };

        let mut comp_env = if eager {
            self.comprehension_entry_from_outer(comp_context.scope, outer_after_iter.clone())
        } else {
            self.deferred_entry_env(comp_context.scope)
        };
        comp_env = self.write_target(&first.target, BindingKind::ForTarget, comp_env);
        for condition in &first.ifs {
            comp_env = self.analyze_expr(condition, comp_context.context, comp_env);
        }
        for generator in generators.iter().skip(1) {
            comp_env = self.analyze_expr(&generator.iter, comp_context.context, comp_env);
            comp_env = self.write_target(&generator.target, BindingKind::ForTarget, comp_env);
            for condition in &generator.ifs {
                comp_env = self.analyze_expr(condition, comp_context.context, comp_env);
            }
        }
        let _ = analyze_result(self, comp_context.context, comp_env);
        outer_after_iter
    }

    fn analyze_interpolated_string(
        &mut self,
        parts: &[FStringPart],
        context: ContextId,
        env: FlowEnv,
    ) -> FlowEnv {
        let mut env = env;
        for part in parts {
            if let FStringPart::FString(string) = part {
                env = self.analyze_interpolated_elements(&string.elements, context, env);
            }
        }
        env
    }

    fn analyze_interpolated_elements(
        &mut self,
        elements: &ruff_python_ast::InterpolatedStringElements,
        context: ContextId,
        env: FlowEnv,
    ) -> FlowEnv {
        let mut env = env;
        for element in elements {
            if let InterpolatedStringElement::Interpolation(interpolation) = element {
                env = self.analyze_expr(&interpolation.expression, context, env);
                if let Some(format_spec) = &interpolation.format_spec {
                    env = self.analyze_interpolated_elements(&format_spec.elements, context, env);
                }
            }
        }
        env
    }

    fn read_target(&mut self, target: &Expr, context: ContextId, env: FlowEnv) -> FlowEnv {
        match target {
            Expr::Name(name) => {
                self.record_reference(context, name.id.as_str(), to_range(name.range), &env);
                env
            }
            Expr::Tuple(tuple) => self.analyze_exprs(&tuple.elts, context, env),
            Expr::List(list) => self.analyze_exprs(&list.elts, context, env),
            Expr::Starred(starred) => self.read_target(&starred.value, context, env),
            Expr::Attribute(attribute) => self.analyze_expr(&attribute.value, context, env),
            Expr::Subscript(subscript) => {
                let env = self.analyze_expr(&subscript.value, context, env);
                self.analyze_expr(&subscript.slice, context, env)
            }
            _ => env,
        }
    }

    fn write_target(&mut self, target: &Expr, kind: BindingKind, env: FlowEnv) -> FlowEnv {
        match target {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store | ExprContext::Del) => {
                self.write_binding_at(to_range(name.range), kind, env)
            }
            Expr::Tuple(tuple) if matches!(tuple.ctx, ExprContext::Store | ExprContext::Del) => {
                tuple
                    .elts
                    .iter()
                    .fold(env, |env, target| self.write_target(target, kind, env))
            }
            Expr::List(list) if matches!(list.ctx, ExprContext::Store | ExprContext::Del) => list
                .elts
                .iter()
                .fold(env, |env, target| self.write_target(target, kind, env)),
            Expr::Starred(starred) => self.write_target(&starred.value, kind, env),
            _ => env,
        }
    }

    fn write_binding_at(&self, range: TextRange, kind: BindingKind, mut env: FlowEnv) -> FlowEnv {
        for binding in self.facts.binding_ids(range, kind) {
            env.write_binding(&self.facts, binding);
        }
        env
    }

    fn pattern_bindings(&self, pattern: &Pattern) -> Vec<BindingId> {
        let mut bindings = Vec::new();
        self.collect_pattern_bindings(pattern, &mut bindings);
        bindings
    }

    fn collect_pattern_bindings(&self, pattern: &Pattern, bindings: &mut Vec<BindingId>) {
        match pattern {
            Pattern::MatchAs(pattern) => {
                if let Some(name) = &pattern.name {
                    bindings.extend(
                        self.facts
                            .binding_ids(to_range(name.range), BindingKind::MatchCapture),
                    );
                }
                if let Some(pattern) = &pattern.pattern {
                    self.collect_pattern_bindings(pattern, bindings);
                }
            }
            Pattern::MatchStar(pattern) => {
                if let Some(name) = &pattern.name {
                    bindings.extend(
                        self.facts
                            .binding_ids(to_range(name.range), BindingKind::MatchCapture),
                    );
                }
            }
            Pattern::MatchSequence(pattern) => {
                for pattern in &pattern.patterns {
                    self.collect_pattern_bindings(pattern, bindings);
                }
            }
            Pattern::MatchMapping(pattern) => {
                if let Some(rest) = &pattern.rest {
                    bindings.extend(
                        self.facts
                            .binding_ids(to_range(rest.range), BindingKind::MatchCapture),
                    );
                }
                for pattern in &pattern.patterns {
                    self.collect_pattern_bindings(pattern, bindings);
                }
            }
            Pattern::MatchClass(pattern) => {
                for pattern in &pattern.arguments.patterns {
                    self.collect_pattern_bindings(pattern, bindings);
                }
                for keyword in &pattern.arguments.keywords {
                    self.collect_pattern_bindings(&keyword.pattern, bindings);
                }
            }
            Pattern::MatchOr(pattern) => {
                for pattern in &pattern.patterns {
                    self.collect_pattern_bindings(pattern, bindings);
                }
            }
            Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
        }
    }

    fn record_reference(
        &mut self,
        context: ContextId,
        source_spelling: &str,
        range: TextRange,
        env: &FlowEnv,
    ) {
        let Some(reference_id) = self.facts.reference(context, range, source_spelling) else {
            return;
        };
        let Some(reference) = self.facts.references.get(&reference_id).cloned() else {
            return;
        };
        let binding_state = if !env.reachable {
            self.binding_state_from_parts(
                LocalReachability::Unreachable,
                BTreeSet::new(),
                ResidualLookup::None,
                BTreeSet::new(),
            )
        } else {
            match &reference.lexical_target {
                Resolution::Resolved(symbol) => {
                    self.evaluate_lookup(env, context, *symbol, &reference.lookup)
                }
                Resolution::Ambiguous(_) | Resolution::External | Resolution::Unresolved(_) => {
                    ReferenceBindingState::NotAnalyzed(FlowFailureReason::UnsupportedFlow)
                }
            }
        };
        self.builder
            .set_reference_binding_state(reference_id, binding_state);
    }

    fn evaluate_lookup(
        &mut self,
        env: &FlowEnv,
        context: ContextId,
        lexical_symbol: SymbolId,
        lookup: &LookupSemantics,
    ) -> ReferenceBindingState {
        match lookup {
            LookupSemantics::Direct => {
                let state = env.get_or_default(&self.facts, context, lexical_symbol);
                self.binding_state_from_slot(
                    LocalReachability::MayExecute,
                    state,
                    LookupMode::Direct,
                )
            }
            LookupSemantics::GlobalThenBuiltin { global_symbol } => {
                let state = env.get_or_default(&self.facts, context, *global_symbol);
                self.binding_state_from_slot(
                    LocalReachability::MayExecute,
                    state,
                    LookupMode::GlobalThenBuiltin,
                )
            }
            LookupSemantics::ClassLocalThenGlobalThenBuiltin {
                class_symbol,
                global_symbol,
            } => {
                let class_state = env.get_or_default(&self.facts, context, *class_symbol);
                let mut bindings = class_state.bindings.clone();
                let mut uncertainties = class_state.uncertainties.clone();
                let mut residual = ResidualLookup::None;

                if class_state.may_be_unbound() {
                    let global_state = env.get_or_default(&self.facts, context, *global_symbol);
                    bindings.extend(global_state.bindings.iter().copied());
                    uncertainties.extend(global_state.uncertainties.iter().copied());
                    residual = residual_for_slot(&global_state, LookupMode::GlobalThenBuiltin);
                }

                self.binding_state_from_parts(
                    LocalReachability::MayExecute,
                    bindings,
                    residual,
                    uncertainties,
                )
            }
        }
    }

    fn binding_state_from_slot(
        &mut self,
        reachability: LocalReachability,
        slot: SlotState,
        mode: LookupMode,
    ) -> ReferenceBindingState {
        let residual = residual_for_slot(&slot, mode);
        self.binding_state_from_parts(reachability, slot.bindings, residual, slot.uncertainties)
    }

    fn binding_state_from_parts(
        &mut self,
        reachability: LocalReachability,
        bindings: BTreeSet<BindingId>,
        residual: ResidualLookup,
        uncertainties: BTreeSet<FlowUncertaintyKind>,
    ) -> ReferenceBindingState {
        let bindings = self.builder.intern_binding_set(bindings);
        let uncertainty = self.builder.intern_flow_uncertainty_set(uncertainties);
        ReferenceBindingState::Analyzed(BindingState {
            reachability,
            bindings,
            residual,
            uncertainty,
        })
    }

    fn module_entry_env(&self) -> FlowEnv {
        let mut env = FlowEnv::reachable();
        if let Some(module_scope) = self.facts.module_scope {
            if let Some(symbols) = self.facts.symbols_by_scope.get(&module_scope) {
                for symbol in symbols {
                    env.slots.insert(*symbol, SlotState::unbound());
                }
            }
        }
        env
    }

    fn class_entry_env(&self, class_scope: ScopeId, outer: FlowEnv) -> FlowEnv {
        let mut env = outer;
        if let Some(symbols) = self.facts.symbols_by_scope.get(&class_scope) {
            for symbol in symbols {
                env.slots.insert(*symbol, SlotState::unbound());
            }
        }
        env
    }

    fn deferred_entry_env(&self, scope: ScopeId) -> FlowEnv {
        let mut env = FlowEnv::reachable();
        for (symbol, symbol_scope) in &self.facts.symbol_scopes {
            let state = if *symbol_scope == scope {
                SlotState::unbound()
            } else if Some(*symbol_scope) == self.facts.module_scope {
                SlotState::runtime_global()
            } else {
                SlotState::runtime_free()
            };
            env.slots.insert(*symbol, state);
        }
        env
    }

    fn comprehension_entry_from_outer(&self, scope: ScopeId, outer: FlowEnv) -> FlowEnv {
        let mut env = outer;
        if let Some(symbols) = self.facts.symbols_by_scope.get(&scope) {
            for symbol in symbols {
                env.slots.insert(*symbol, SlotState::unbound());
            }
        }
        env
    }

    fn merge_class_side_effects(
        &self,
        class_scope: ScopeId,
        mut outer: FlowEnv,
        class_env: FlowEnv,
    ) -> FlowEnv {
        for (symbol, state) in class_env.slots {
            if self.facts.symbol_scopes.get(&symbol).copied() != Some(class_scope) {
                outer.slots.insert(symbol, state);
            }
        }
        outer
    }

    fn apply_call_barrier(
        &self,
        env: &mut FlowEnv,
        context: ContextId,
        override_uncertainty: Option<FlowUncertaintyKind>,
    ) {
        let global_uncertainty =
            override_uncertainty.unwrap_or(FlowUncertaintyKind::OpaqueCallMayMutateGlobal);
        let closure_uncertainty =
            override_uncertainty.unwrap_or(FlowUncertaintyKind::OpaqueCallMayMutateClosure);
        let current_scope = self.facts.context_scope(context);
        let symbols = env.slots.keys().copied().collect::<Vec<_>>();
        for symbol in symbols {
            let Some(scope) = self.facts.symbol_scopes.get(&symbol).copied() else {
                continue;
            };
            if self.facts.scope_kind(scope) == Some(ScopeKind::Module) {
                env.add_uncertainty_to_symbol(
                    symbol,
                    SlotResidual::RuntimeGlobalThenBuiltin,
                    global_uncertainty,
                );
            } else if Some(scope) != current_scope {
                env.add_uncertainty_to_symbol(
                    symbol,
                    SlotResidual::RuntimeFreeVariable,
                    closure_uncertainty,
                );
            }
        }
    }

    fn mark_unvisited_references_not_analyzed(&mut self, module: ModuleId) {
        let reference_ids = self
            .builder
            .graph()
            .references
            .iter()
            .filter(|reference| reference.module == module)
            .filter(|reference| reference.binding_state == ReferenceBindingState::NotApplicable)
            .map(|reference| reference.id)
            .collect::<Vec<_>>();
        for reference in reference_ids {
            self.builder.set_reference_binding_state(
                reference,
                ReferenceBindingState::NotAnalyzed(FlowFailureReason::UnsupportedFlow),
            );
        }
    }

    fn mark_context_unsupported(&mut self, context: ContextId, reason: FlowFailureReason) {
        self.unsupported_contexts.entry(context).or_insert(reason);
    }

    fn finalize_context_statuses(&mut self) {
        let contexts = self.facts.contexts.keys().copied().collect::<Vec<_>>();
        for context in contexts {
            if let Some(reason) = self.unsupported_contexts.get(&context).cloned() {
                self.builder.set_context_flow_status(
                    context,
                    ContextFlowStatus::Unsupported(reason.clone()),
                );
                let reference_ids = self
                    .builder
                    .graph()
                    .references
                    .iter()
                    .filter(|reference| reference.source_context == context)
                    .map(|reference| reference.id)
                    .collect::<Vec<_>>();
                for reference in reference_ids {
                    self.builder.set_reference_binding_state(
                        reference,
                        ReferenceBindingState::NotAnalyzed(reason.clone()),
                    );
                }
            } else {
                self.builder
                    .set_context_flow_status(context, ContextFlowStatus::Complete);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum LookupMode {
    Direct,
    GlobalThenBuiltin,
}

fn residual_for_slot(slot: &SlotState, mode: LookupMode) -> ResidualLookup {
    if slot
        .residuals
        .contains(&SlotResidual::RuntimeGlobalThenBuiltin)
    {
        return ResidualLookup::RuntimeGlobalThenBuiltin;
    }
    if slot.residuals.contains(&SlotResidual::RuntimeFreeVariable) {
        return ResidualLookup::RuntimeFreeVariable;
    }
    if slot.residuals.contains(&SlotResidual::BuiltinOrNameError) {
        return ResidualLookup::BuiltinOrNameError;
    }
    if slot.residuals.contains(&SlotResidual::Unbound) {
        return match mode {
            LookupMode::Direct => ResidualLookup::UnboundLocal,
            LookupMode::GlobalThenBuiltin => ResidualLookup::BuiltinOrNameError,
        };
    }
    ResidualLookup::None
}

fn join_envs(left: Option<FlowEnv>, right: Option<FlowEnv>) -> Option<FlowEnv> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.join(&right)),
        (Some(left), None) => Some(left),
        (None, Some(right)) => Some(right),
        (None, None) => None,
    }
}
