use cull_core::{
    BindingId, BindingInput, BindingKind, ContextId, ContextKind, DefinitionInput, DefinitionKind,
    ModuleId, ScopeContextInput, ScopeId, ScopeKind, SemanticGraphBuilder, SemanticModule,
    SymbolId, TextRange,
};
use ruff_python_ast::{
    Expr, ExprContext, ModModule, Parameter, Stmt, StmtClassDef, StmtFunctionDef,
};

use crate::{
    frontend::ParseInput,
    ruff_frontend::{definition_statement_range, module_has_future_annotations, to_range},
};

pub(crate) fn collect_module_bindings(
    builder: &mut SemanticGraphBuilder,
    input: ParseInput<'_>,
    module: &ModModule,
) {
    let module_range = to_range(module.range);
    let (scope, context) = builder.add_scope_with_context(ScopeContextInput {
        module: input.module_id,
        scope_kind: ScopeKind::Module,
        context_kind: ContextKind::ModuleBody,
        parent_scope: None,
        parent_context: None,
        owner_definition: None,
        name: input.module_name.to_owned(),
        range: module_range,
    });

    builder.add_module(SemanticModule {
        id: input.module_id,
        file: input.file_id,
        name: input.module_name.to_owned(),
        path: input.display_path.to_owned(),
        future_annotations: module_has_future_annotations(&module.body),
        scope,
        context,
    });

    let mut collector = ModuleCollector {
        builder,
        module: input.module_id,
        source: &input.source.text,
    };
    collector.collect_body(
        &module.body,
        ActiveScope {
            scope,
            context,
            kind: ScopeKind::Module,
            lexical_parent_for_nested_scopes: scope,
            qualified_name: input.module_name.to_owned(),
        },
    );
}

#[derive(Clone, Debug)]
struct ActiveScope {
    scope: ScopeId,
    context: ContextId,
    kind: ScopeKind,
    lexical_parent_for_nested_scopes: ScopeId,
    qualified_name: String,
}

struct ModuleCollector<'a, 'b> {
    builder: &'a mut SemanticGraphBuilder,
    module: ModuleId,
    source: &'b str,
}

impl ModuleCollector<'_, '_> {
    fn collect_body(&mut self, statements: &[Stmt], active: ActiveScope) {
        for statement in statements {
            self.collect_statement(statement, active.clone());
        }
    }

    fn collect_statement(&mut self, statement: &Stmt, active: ActiveScope) {
        match statement {
            Stmt::FunctionDef(function) => self.collect_function(function, active),
            Stmt::ClassDef(class) => self.collect_class(class, active),
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    self.collect_target(target, &active, BindingKind::Assignment);
                }
            }
            Stmt::AnnAssign(assign) => {
                if assign.value.is_some() {
                    self.collect_target(&assign.target, &active, BindingKind::AnnotatedAssignment);
                }
            }
            Stmt::AugAssign(assign) => {
                self.collect_target(&assign.target, &active, BindingKind::AugmentedAssignment);
            }
            Stmt::Import(import) => {
                for alias in &import.names {
                    let bound_name = alias
                        .asname
                        .as_ref()
                        .map(|name| name.id.to_string())
                        .unwrap_or_else(|| import_root_name(alias.name.id.as_str()));
                    let name_range = alias
                        .asname
                        .as_ref()
                        .map(|name| to_range(name.range))
                        .unwrap_or_else(|| to_range(alias.name.range));
                    self.bind_name(
                        &active,
                        BindingKind::Import,
                        bound_name,
                        to_range(alias.range),
                        name_range,
                    );
                }
            }
            Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    if alias.name.id.as_str() == "*" {
                        continue;
                    }
                    let bound_name = alias
                        .asname
                        .as_ref()
                        .map(|name| name.id.to_string())
                        .unwrap_or_else(|| alias.name.id.to_string());
                    let name_range = alias
                        .asname
                        .as_ref()
                        .map(|name| to_range(name.range))
                        .unwrap_or_else(|| to_range(alias.name.range));
                    self.bind_name(
                        &active,
                        BindingKind::ImportFrom,
                        bound_name,
                        to_range(alias.range),
                        name_range,
                    );
                }
            }
            Stmt::TypeAlias(alias) => {
                self.collect_target(&alias.name, &active, BindingKind::TypeAlias);
            }
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.collect_target(target, &active, BindingKind::Delete);
                }
            }
            Stmt::For(for_stmt) => {
                self.collect_target(&for_stmt.target, &active, BindingKind::ForTarget);
                self.collect_body(&for_stmt.body, active.clone());
                self.collect_body(&for_stmt.orelse, active);
            }
            Stmt::While(while_stmt) => {
                self.collect_body(&while_stmt.body, active.clone());
                self.collect_body(&while_stmt.orelse, active);
            }
            Stmt::If(if_stmt) => {
                self.collect_body(&if_stmt.body, active.clone());
                for clause in &if_stmt.elif_else_clauses {
                    self.collect_body(&clause.body, active.clone());
                }
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    if let Some(target) = &item.optional_vars {
                        self.collect_target(target, &active, BindingKind::WithTarget);
                    }
                }
                self.collect_body(&with_stmt.body, active);
            }
            Stmt::Try(try_stmt) => {
                self.collect_body(&try_stmt.body, active.clone());
                for handler in &try_stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(name) = &handler.name {
                        self.bind_name(
                            &active,
                            BindingKind::ExceptTarget,
                            name.id.to_string(),
                            to_range(name.range),
                            to_range(name.range),
                        );
                    }
                    self.collect_body(&handler.body, active.clone());
                }
                self.collect_body(&try_stmt.orelse, active.clone());
                self.collect_body(&try_stmt.finalbody, active);
            }
            Stmt::Match(match_stmt) => {
                for case in &match_stmt.cases {
                    self.collect_body(&case.body, active.clone());
                }
            }
            Stmt::Return(_)
            | Stmt::Raise(_)
            | Stmt::Assert(_)
            | Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Expr(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    fn collect_function(&mut self, function: &StmtFunctionDef, active: ActiveScope) {
        let name = function.name.id.to_string();
        let name_range = to_range(function.name.range);
        let range = definition_statement_range(
            self.source,
            to_range(function.range),
            name_range,
            function.is_async,
            DefinitionKind::Function,
        );
        let binding = self.bind_name(
            &active,
            BindingKind::FunctionDefinition,
            name.clone(),
            range,
            name_range,
        );

        let qualified_name = child_qualified_name(&active.qualified_name, &name);
        let (scope, context) = self.builder.add_scope_with_context(ScopeContextInput {
            module: self.module,
            scope_kind: ScopeKind::Function,
            context_kind: ContextKind::FunctionBody,
            parent_scope: Some(active.lexical_parent_for_nested_scopes),
            parent_context: Some(active.context),
            owner_definition: None,
            name: qualified_name.clone(),
            range,
        });

        self.builder.add_definition(DefinitionInput {
            module: self.module,
            binding,
            scope,
            context,
            kind: DefinitionKind::Function,
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            range,
            name_range,
            reportable: active.kind == ScopeKind::Module,
            is_async: function.is_async,
        });

        let child = ActiveScope {
            scope,
            context,
            kind: ScopeKind::Function,
            lexical_parent_for_nested_scopes: scope,
            qualified_name,
        };
        self.collect_parameters(&function.parameters, &child);
        self.collect_body(&function.body, child);
    }

    fn collect_class(&mut self, class: &StmtClassDef, active: ActiveScope) {
        let name = class.name.id.to_string();
        let name_range = to_range(class.name.range);
        let range = definition_statement_range(
            self.source,
            to_range(class.range),
            name_range,
            false,
            DefinitionKind::Class,
        );
        let binding = self.bind_name(
            &active,
            BindingKind::ClassDefinition,
            name.clone(),
            range,
            name_range,
        );

        let qualified_name = child_qualified_name(&active.qualified_name, &name);
        let (scope, context) = self.builder.add_scope_with_context(ScopeContextInput {
            module: self.module,
            scope_kind: ScopeKind::Class,
            context_kind: ContextKind::ClassBody,
            parent_scope: Some(active.lexical_parent_for_nested_scopes),
            parent_context: Some(active.context),
            owner_definition: None,
            name: qualified_name.clone(),
            range,
        });

        self.builder.add_definition(DefinitionInput {
            module: self.module,
            binding,
            scope,
            context,
            kind: DefinitionKind::Class,
            name: name.clone(),
            qualified_name: qualified_name.clone(),
            range,
            name_range,
            reportable: active.kind == ScopeKind::Module,
            is_async: false,
        });

        let child = ActiveScope {
            scope,
            context,
            kind: ScopeKind::Class,
            lexical_parent_for_nested_scopes: active.lexical_parent_for_nested_scopes,
            qualified_name,
        };
        self.collect_body(&class.body, child);
    }

    fn collect_parameters(
        &mut self,
        parameters: &ruff_python_ast::Parameters,
        active: &ActiveScope,
    ) {
        for parameter in parameters.iter_source_order() {
            let parameter = parameter.as_parameter();
            self.bind_parameter(active, parameter);
        }
    }

    fn bind_parameter(&mut self, active: &ActiveScope, parameter: &Parameter) {
        self.bind_name(
            active,
            BindingKind::Parameter,
            parameter.name.id.to_string(),
            to_range(parameter.range),
            to_range(parameter.name.range),
        );
    }

    fn collect_target(&mut self, target: &Expr, active: &ActiveScope, kind: BindingKind) {
        match target {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store | ExprContext::Del) => {
                self.bind_name(
                    active,
                    kind,
                    name.id.to_string(),
                    to_range(name.range),
                    to_range(name.range),
                );
            }
            Expr::Tuple(tuple) if matches!(tuple.ctx, ExprContext::Store | ExprContext::Del) => {
                for element in &tuple.elts {
                    self.collect_target(element, active, kind);
                }
            }
            Expr::List(list) if matches!(list.ctx, ExprContext::Store | ExprContext::Del) => {
                for element in &list.elts {
                    self.collect_target(element, active, kind);
                }
            }
            Expr::Starred(starred) => {
                self.collect_target(&starred.value, active, kind);
            }
            _ => {}
        }
    }

    fn bind_name(
        &mut self,
        active: &ActiveScope,
        kind: BindingKind,
        name: String,
        range: TextRange,
        name_range: TextRange,
    ) -> BindingId {
        let symbol = self.symbol(active, &name);
        self.builder.add_binding(BindingInput {
            module: self.module,
            scope: active.scope,
            symbol,
            kind,
            name,
            range,
            name_range,
        })
    }

    fn symbol(&mut self, active: &ActiveScope, name: &str) -> SymbolId {
        self.builder.symbol(self.module, active.scope, name)
    }
}

fn import_root_name(name: &str) -> String {
    name.split('.').next().unwrap_or(name).to_owned()
}

fn child_qualified_name(parent: &str, name: &str) -> String {
    if parent.contains("::") {
        format!("{parent}.{name}")
    } else {
        format!("{parent}::{name}")
    }
}
