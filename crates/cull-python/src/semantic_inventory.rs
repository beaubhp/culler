use std::collections::{BTreeMap, BTreeSet};

use cull_core::{
    BindingId, BindingInput, BindingKind, ContextId, ContextKind, DefinitionInput, DefinitionKind,
    Diagnostic, LookupSemantics, ModuleId, OriginDomain, ReferenceBindingState, ReferenceInput,
    ReferencePhase, ReferenceRole, Resolution, ScopeContextInput, ScopeId, ScopeKind,
    SemanticGraphBuilder, SemanticModule, SymbolId, TextRange, UnresolvedReason,
};
use ruff_python_ast::{
    Expr, ExprContext, FStringPart, Identifier, InterpolatedStringElement, ModModule, Parameter,
    Pattern, Stmt, StmtClassDef, StmtFunctionDef,
};

use crate::{
    frontend::ParseInput,
    ruff_frontend::{definition_statement_range, module_has_future_annotations, to_range},
};

pub(crate) fn collect_module_semantics(
    builder: &mut SemanticGraphBuilder,
    diagnostics: &mut Vec<Diagnostic>,
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

    let declarations = collect_body_declarations(&module.body, None);
    let mut collector = ModuleCollector {
        builder,
        diagnostics,
        module: input.module_id,
        display_path: input.display_path,
        source: &input.source.text,
    };
    collector.predeclare(scope, &declarations);
    collector.collect_body(
        &module.body,
        ActiveScope {
            scope,
            context,
            kind: ScopeKind::Module,
            lexical_parent_for_nested_scopes: scope,
            qualified_name: input.module_name.to_owned(),
            declarations,
            lexical_frames: Vec::new(),
            module_scope: scope,
            class_mangle: None,
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
    declarations: BlockDeclarations,
    lexical_frames: Vec<LexicalFrame>,
    module_scope: ScopeId,
    class_mangle: Option<String>,
}

#[derive(Clone, Debug)]
struct LexicalFrame {
    scope: ScopeId,
    kind: ScopeKind,
    declarations: BlockDeclarations,
}

#[derive(Clone, Debug, Default)]
struct BlockDeclarations {
    locals: BTreeSet<String>,
    globals: BTreeMap<String, TextRange>,
    nonlocals: BTreeMap<String, TextRange>,
    parameters: BTreeSet<String>,
    first_bindings: BTreeMap<String, TextRange>,
    first_loads: BTreeMap<String, TextRange>,
}

impl BlockDeclarations {
    fn is_global(&self, name: &str) -> bool {
        self.globals.contains_key(name)
    }

    fn is_nonlocal(&self, name: &str) -> bool {
        self.nonlocals.contains_key(name)
    }

    fn record_binding(&mut self, name: String, range: TextRange) {
        self.first_bindings.entry(name.clone()).or_insert(range);
        self.locals.insert(name);
    }

    fn record_global(&mut self, name: String, range: TextRange) {
        self.globals.entry(name).or_insert(range);
    }

    fn record_load(&mut self, name: String, range: TextRange) {
        self.first_loads.entry(name).or_insert(range);
    }

    fn record_nonlocal(&mut self, name: String, range: TextRange) {
        self.nonlocals.entry(name).or_insert(range);
    }

    fn record_parameter(&mut self, name: String, range: TextRange) {
        self.parameters.insert(name.clone());
        self.record_binding(name, range);
    }

    fn finish(&mut self) {
        for name in self.globals.keys().chain(self.nonlocals.keys()) {
            self.locals.remove(name);
        }
    }
}

struct ModuleCollector<'a, 'b> {
    builder: &'a mut SemanticGraphBuilder,
    diagnostics: &'a mut Vec<Diagnostic>,
    module: ModuleId,
    display_path: &'b str,
    source: &'b str,
}

impl ModuleCollector<'_, '_> {
    fn collect_body(&mut self, statements: &[Stmt], active: ActiveScope) {
        self.validate_declarations(&active);
        for statement in statements {
            self.collect_statement(statement, active.clone());
        }
    }

    fn collect_statement(&mut self, statement: &Stmt, active: ActiveScope) {
        match statement {
            Stmt::FunctionDef(function) => self.collect_function(function, active),
            Stmt::ClassDef(class) => self.collect_class(class, active),
            Stmt::Return(return_stmt) => {
                if let Some(value) = &return_stmt.value {
                    self.collect_expr(value, &active, ReferenceRole::Value);
                }
            }
            Stmt::Delete(delete) => {
                for target in &delete.targets {
                    self.collect_target(target, &active, BindingKind::Delete);
                }
            }
            Stmt::Assign(assign) => {
                self.collect_expr(&assign.value, &active, ReferenceRole::Value);
                for target in &assign.targets {
                    self.collect_target(target, &active, BindingKind::Assignment);
                }
            }
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.collect_expr(value, &active, ReferenceRole::Value);
                }
                self.collect_target(&assign.target, &active, BindingKind::AnnotatedAssignment);
            }
            Stmt::AugAssign(assign) => {
                self.collect_target_reference(&assign.target, &active, ReferenceRole::Value);
                self.collect_expr(&assign.value, &active, ReferenceRole::Value);
                self.collect_target(&assign.target, &active, BindingKind::AugmentedAssignment);
            }
            Stmt::For(for_stmt) => {
                self.collect_expr(&for_stmt.iter, &active, ReferenceRole::Value);
                self.collect_target(&for_stmt.target, &active, BindingKind::ForTarget);
                self.collect_body(&for_stmt.body, active.clone());
                self.collect_body(&for_stmt.orelse, active);
            }
            Stmt::While(while_stmt) => {
                self.collect_expr(&while_stmt.test, &active, ReferenceRole::Value);
                self.collect_body(&while_stmt.body, active.clone());
                self.collect_body(&while_stmt.orelse, active);
            }
            Stmt::If(if_stmt) => {
                self.collect_expr(&if_stmt.test, &active, ReferenceRole::Value);
                self.collect_body(&if_stmt.body, active.clone());
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.collect_expr(test, &active, ReferenceRole::Value);
                    }
                    self.collect_body(&clause.body, active.clone());
                }
            }
            Stmt::With(with_stmt) => {
                for item in &with_stmt.items {
                    self.collect_expr(&item.context_expr, &active, ReferenceRole::Value);
                    if let Some(target) = &item.optional_vars {
                        self.collect_target(target, &active, BindingKind::WithTarget);
                    }
                }
                self.collect_body(&with_stmt.body, active);
            }
            Stmt::Match(match_stmt) => {
                self.collect_expr(&match_stmt.subject, &active, ReferenceRole::Value);
                for case in &match_stmt.cases {
                    self.collect_pattern(&case.pattern, &active);
                    if let Some(guard) = &case.guard {
                        self.collect_expr(guard, &active, ReferenceRole::Value);
                    }
                    self.collect_body(&case.body, active.clone());
                }
            }
            Stmt::Raise(raise) => {
                if let Some(exc) = &raise.exc {
                    self.collect_expr(exc, &active, ReferenceRole::Value);
                }
                if let Some(cause) = &raise.cause {
                    self.collect_expr(cause, &active, ReferenceRole::Value);
                }
            }
            Stmt::Try(try_stmt) => {
                self.collect_body(&try_stmt.body, active.clone());
                for handler in &try_stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(type_) = &handler.type_ {
                        self.collect_expr(type_, &active, ReferenceRole::Value);
                    }
                    if let Some(name) = &handler.name {
                        self.bind_identifier(&active, BindingKind::ExceptTarget, name);
                    }
                    self.collect_body(&handler.body, active.clone());
                }
                self.collect_body(&try_stmt.orelse, active.clone());
                self.collect_body(&try_stmt.finalbody, active);
            }
            Stmt::Assert(assert_stmt) => {
                self.collect_expr(&assert_stmt.test, &active, ReferenceRole::Value);
                if let Some(msg) = &assert_stmt.msg {
                    self.collect_expr(msg, &active, ReferenceRole::Value);
                }
            }
            Stmt::Import(import) => {
                for alias in &import.names {
                    let (bound_name, name_range) = alias
                        .asname
                        .as_ref()
                        .map(|name| {
                            (
                                self.semantic_name(&active, name.id.as_str()),
                                to_range(name.range),
                            )
                        })
                        .unwrap_or_else(|| {
                            (
                                self.semantic_name(
                                    &active,
                                    &import_root_name(alias.name.id.as_str()),
                                ),
                                to_range(alias.name.range),
                            )
                        });
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
                    let (bound_name, name_range) = alias
                        .asname
                        .as_ref()
                        .map(|name| {
                            (
                                self.semantic_name(&active, name.id.as_str()),
                                to_range(name.range),
                            )
                        })
                        .unwrap_or_else(|| {
                            (
                                self.semantic_name(&active, alias.name.id.as_str()),
                                to_range(alias.name.range),
                            )
                        });
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
            Stmt::Expr(expr) => {
                self.collect_expr(&expr.value, &active, ReferenceRole::Value);
            }
            Stmt::Global(_)
            | Stmt::Nonlocal(_)
            | Stmt::Pass(_)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    fn collect_function(&mut self, function: &StmtFunctionDef, active: ActiveScope) {
        for decorator in &function.decorator_list {
            self.collect_expr(&decorator.expression, &active, ReferenceRole::Decorator);
        }
        self.collect_parameter_defaults(&function.parameters, &active);

        let source_name = function.name.id.to_string();
        let semantic_name = self.semantic_name(&active, &source_name);
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
            semantic_name.clone(),
            range,
            name_range,
        );

        let declarations = collect_function_declarations(function, active.class_mangle.as_deref());
        let qualified_name = child_qualified_name(&active.qualified_name, &semantic_name);
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
        self.predeclare(scope, &declarations);

        self.builder.add_definition(DefinitionInput {
            module: self.module,
            binding,
            scope,
            context,
            kind: DefinitionKind::Function,
            name: semantic_name.clone(),
            qualified_name: qualified_name.clone(),
            range,
            name_range,
            reportable: active.kind == ScopeKind::Module,
            is_async: function.is_async,
        });

        let mut frames = active.lexical_frames.clone();
        frames.push(LexicalFrame {
            scope,
            kind: ScopeKind::Function,
            declarations: declarations.clone(),
        });
        let child = ActiveScope {
            scope,
            context,
            kind: ScopeKind::Function,
            lexical_parent_for_nested_scopes: scope,
            qualified_name,
            declarations,
            lexical_frames: frames,
            module_scope: active.module_scope,
            class_mangle: active.class_mangle,
        };
        self.collect_parameters(&function.parameters, &child);
        self.collect_body(&function.body, child);
    }

    fn collect_class(&mut self, class: &StmtClassDef, active: ActiveScope) {
        for decorator in &class.decorator_list {
            self.collect_expr(&decorator.expression, &active, ReferenceRole::Decorator);
        }
        if let Some(arguments) = &class.arguments {
            for base in &arguments.args {
                self.collect_expr(base, &active, ReferenceRole::BaseClass);
            }
            for keyword in &arguments.keywords {
                self.collect_expr(&keyword.value, &active, ReferenceRole::ClassKeyword);
            }
        }

        let source_name = class.name.id.to_string();
        let semantic_name = self.semantic_name(&active, &source_name);
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
            semantic_name.clone(),
            range,
            name_range,
        );

        let class_mangle = Some(mangle_class_name(&source_name));
        let declarations = collect_body_declarations(&class.body, class_mangle.as_deref());
        let qualified_name = child_qualified_name(&active.qualified_name, &semantic_name);
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
        self.predeclare(scope, &declarations);

        self.builder.add_definition(DefinitionInput {
            module: self.module,
            binding,
            scope,
            context,
            kind: DefinitionKind::Class,
            name: semantic_name.clone(),
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
            declarations,
            lexical_frames: active.lexical_frames,
            module_scope: active.module_scope,
            class_mangle,
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

    fn collect_parameter_defaults(
        &mut self,
        parameters: &ruff_python_ast::Parameters,
        active: &ActiveScope,
    ) {
        for parameter in parameters.iter_source_order() {
            if let Some(default) = parameter.default() {
                self.collect_expr(default, active, ReferenceRole::DefaultValue);
            }
        }
    }

    fn bind_parameter(&mut self, active: &ActiveScope, parameter: &Parameter) {
        let source_name = parameter.name.id.as_str();
        let semantic_name = self.semantic_name(active, source_name);
        let symbol = self
            .builder
            .symbol(self.module, active.scope, &semantic_name);
        self.builder.add_binding(BindingInput {
            module: self.module,
            scope: active.scope,
            symbol,
            kind: BindingKind::Parameter,
            name: semantic_name,
            range: to_range(parameter.range),
            name_range: to_range(parameter.name.range),
        });
    }

    fn collect_expr(&mut self, expression: &Expr, active: &ActiveScope, role: ReferenceRole) {
        match expression {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                self.add_name_reference(active, name.id.as_str(), to_range(name.range), role);
            }
            Expr::Name(_) => {}
            Expr::BoolOp(expr) => {
                for value in &expr.values {
                    self.collect_expr(value, active, role);
                }
            }
            Expr::Named(expr) => {
                self.collect_expr(&expr.value, active, role);
                if active.kind == ScopeKind::Comprehension {
                    self.diagnostics.push(
                        Diagnostic::warning(
                            "CULL_P1105",
                            "assignment expressions inside comprehensions are deferred to a later semantic slice",
                        )
                        .with_path(self.display_path.to_owned())
                        .with_range(to_range(expr.range)),
                    );
                    return;
                }
                self.collect_target(&expr.target, active, BindingKind::NamedExpression);
            }
            Expr::BinOp(expr) => {
                self.collect_expr(&expr.left, active, role);
                self.collect_expr(&expr.right, active, role);
            }
            Expr::UnaryOp(expr) => self.collect_expr(&expr.operand, active, role),
            Expr::Lambda(lambda) => self.collect_lambda(lambda, active),
            Expr::If(expr) => {
                self.collect_expr(&expr.test, active, role);
                self.collect_expr(&expr.body, active, role);
                self.collect_expr(&expr.orelse, active, role);
            }
            Expr::Dict(expr) => {
                for item in &expr.items {
                    if let Some(key) = &item.key {
                        self.collect_expr(key, active, role);
                    }
                    self.collect_expr(&item.value, active, role);
                }
            }
            Expr::Set(expr) => {
                for element in &expr.elts {
                    self.collect_expr(element, active, role);
                }
            }
            Expr::ListComp(expr) => {
                self.collect_comprehension(
                    active,
                    to_range(expr.range),
                    &expr.generators,
                    |collector, child| {
                        collector.collect_expr(&expr.elt, child, ReferenceRole::Value);
                    },
                );
            }
            Expr::SetComp(expr) => {
                self.collect_comprehension(
                    active,
                    to_range(expr.range),
                    &expr.generators,
                    |collector, child| {
                        collector.collect_expr(&expr.elt, child, ReferenceRole::Value);
                    },
                );
            }
            Expr::DictComp(expr) => {
                self.collect_comprehension(
                    active,
                    to_range(expr.range),
                    &expr.generators,
                    |collector, child| {
                        if let Some(key) = &expr.key {
                            collector.collect_expr(key, child, ReferenceRole::Value);
                        }
                        collector.collect_expr(&expr.value, child, ReferenceRole::Value);
                    },
                );
            }
            Expr::Generator(expr) => {
                self.collect_comprehension(
                    active,
                    to_range(expr.range),
                    &expr.generators,
                    |collector, child| {
                        collector.collect_expr(&expr.elt, child, ReferenceRole::Value);
                    },
                );
            }
            Expr::Await(expr) => self.collect_expr(&expr.value, active, role),
            Expr::Yield(expr) => {
                if let Some(value) = &expr.value {
                    self.collect_expr(value, active, role);
                }
            }
            Expr::YieldFrom(expr) => self.collect_expr(&expr.value, active, role),
            Expr::Compare(expr) => {
                self.collect_expr(&expr.left, active, role);
                for comparator in &expr.comparators {
                    self.collect_expr(comparator, active, role);
                }
            }
            Expr::Call(expr) => {
                self.collect_expr(&expr.func, active, role);
                for arg in &expr.arguments.args {
                    self.collect_expr(arg, active, role);
                }
                for keyword in &expr.arguments.keywords {
                    self.collect_expr(&keyword.value, active, role);
                }
            }
            Expr::Attribute(expr) => self.collect_expr(&expr.value, active, role),
            Expr::Subscript(expr) => {
                self.collect_expr(&expr.value, active, role);
                self.collect_expr(&expr.slice, active, role);
            }
            Expr::Starred(expr) => self.collect_expr(&expr.value, active, role),
            Expr::List(expr) => {
                for element in &expr.elts {
                    self.collect_expr(element, active, role);
                }
            }
            Expr::Tuple(expr) => {
                for element in &expr.elts {
                    self.collect_expr(element, active, role);
                }
            }
            Expr::Slice(expr) => {
                if let Some(lower) = &expr.lower {
                    self.collect_expr(lower, active, role);
                }
                if let Some(upper) = &expr.upper {
                    self.collect_expr(upper, active, role);
                }
                if let Some(step) = &expr.step {
                    self.collect_expr(step, active, role);
                }
            }
            Expr::FString(expr) => {
                self.collect_interpolated_string_expr(expr.value.as_slice(), active, role)
            }
            Expr::TString(expr) => {
                for string in expr.value.as_slice() {
                    self.collect_interpolated_elements(&string.elements, active, role);
                }
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::IpyEscapeCommand(_) => {}
        }
    }

    fn collect_interpolated_string_expr(
        &mut self,
        parts: &[FStringPart],
        active: &ActiveScope,
        role: ReferenceRole,
    ) {
        for part in parts {
            if let FStringPart::FString(string) = part {
                self.collect_interpolated_elements(&string.elements, active, role);
            }
        }
    }

    fn collect_interpolated_elements(
        &mut self,
        elements: &ruff_python_ast::InterpolatedStringElements,
        active: &ActiveScope,
        role: ReferenceRole,
    ) {
        for element in elements {
            if let InterpolatedStringElement::Interpolation(interpolation) = element {
                self.collect_expr(&interpolation.expression, active, role);
                if let Some(format_spec) = &interpolation.format_spec {
                    self.collect_interpolated_elements(&format_spec.elements, active, role);
                }
            }
        }
    }

    fn collect_lambda(&mut self, lambda: &ruff_python_ast::ExprLambda, active: &ActiveScope) {
        if let Some(parameters) = &lambda.parameters {
            self.collect_parameter_defaults(parameters, active);
        }

        let declarations = collect_lambda_declarations(lambda, active.class_mangle.as_deref());
        let name = format!(
            "{}::<lambda@{}>",
            active.qualified_name,
            lambda.range.start().to_u32()
        );
        let (scope, context) = self.builder.add_scope_with_context(ScopeContextInput {
            module: self.module,
            scope_kind: ScopeKind::Lambda,
            context_kind: ContextKind::LambdaBody,
            parent_scope: Some(active.lexical_parent_for_nested_scopes),
            parent_context: Some(active.context),
            owner_definition: None,
            name,
            range: to_range(lambda.range),
        });
        self.predeclare(scope, &declarations);

        let mut frames = active.lexical_frames.clone();
        frames.push(LexicalFrame {
            scope,
            kind: ScopeKind::Lambda,
            declarations: declarations.clone(),
        });
        let child = ActiveScope {
            scope,
            context,
            kind: ScopeKind::Lambda,
            lexical_parent_for_nested_scopes: scope,
            qualified_name: active.qualified_name.clone(),
            declarations,
            lexical_frames: frames,
            module_scope: active.module_scope,
            class_mangle: active.class_mangle.clone(),
        };
        if let Some(parameters) = &lambda.parameters {
            self.collect_parameters(parameters, &child);
        }
        self.collect_expr(&lambda.body, &child, ReferenceRole::Value);
    }

    fn collect_comprehension<F>(
        &mut self,
        active: &ActiveScope,
        range: TextRange,
        generators: &[ruff_python_ast::Comprehension],
        mut collect_result: F,
    ) where
        F: FnMut(&mut Self, &ActiveScope),
    {
        let Some(first) = generators.first() else {
            return;
        };
        self.collect_expr(&first.iter, active, ReferenceRole::ComprehensionIterable);

        let declarations =
            collect_comprehension_declarations(generators, active.class_mangle.as_deref());
        let name = format!("{}::<comp@{}>", active.qualified_name, range.start);
        let (scope, context) = self.builder.add_scope_with_context(ScopeContextInput {
            module: self.module,
            scope_kind: ScopeKind::Comprehension,
            context_kind: ContextKind::ComprehensionBody,
            parent_scope: Some(active.lexical_parent_for_nested_scopes),
            parent_context: Some(active.context),
            owner_definition: None,
            name,
            range,
        });
        self.predeclare(scope, &declarations);

        let mut frames = active.lexical_frames.clone();
        frames.push(LexicalFrame {
            scope,
            kind: ScopeKind::Comprehension,
            declarations: declarations.clone(),
        });
        let child = ActiveScope {
            scope,
            context,
            kind: ScopeKind::Comprehension,
            lexical_parent_for_nested_scopes: scope,
            qualified_name: active.qualified_name.clone(),
            declarations,
            lexical_frames: frames,
            module_scope: active.module_scope,
            class_mangle: active.class_mangle.clone(),
        };

        self.collect_target(&first.target, &child, BindingKind::ForTarget);
        for condition in &first.ifs {
            self.collect_expr(condition, &child, ReferenceRole::Value);
        }
        for generator in generators.iter().skip(1) {
            self.collect_expr(&generator.iter, &child, ReferenceRole::Value);
            self.collect_target(&generator.target, &child, BindingKind::ForTarget);
            for condition in &generator.ifs {
                self.collect_expr(condition, &child, ReferenceRole::Value);
            }
        }
        collect_result(self, &child);
    }

    fn collect_target_reference(
        &mut self,
        target: &Expr,
        active: &ActiveScope,
        role: ReferenceRole,
    ) {
        match target {
            Expr::Name(name) => {
                self.add_name_reference(active, name.id.as_str(), to_range(name.range), role);
            }
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.collect_target_reference(element, active, role);
                }
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.collect_target_reference(element, active, role);
                }
            }
            Expr::Starred(starred) => self.collect_target_reference(&starred.value, active, role),
            Expr::Attribute(attribute) => self.collect_expr(&attribute.value, active, role),
            Expr::Subscript(subscript) => {
                self.collect_expr(&subscript.value, active, role);
                self.collect_expr(&subscript.slice, active, role);
            }
            _ => {}
        }
    }

    fn collect_target(&mut self, target: &Expr, active: &ActiveScope, kind: BindingKind) {
        match target {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Store | ExprContext::Del) => {
                let semantic_name = self.semantic_name(active, name.id.as_str());
                self.bind_name(
                    active,
                    kind,
                    semantic_name,
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
            Expr::Starred(starred) => self.collect_target(&starred.value, active, kind),
            _ => {}
        }
    }

    fn collect_pattern(&mut self, pattern: &Pattern, active: &ActiveScope) {
        match pattern {
            Pattern::MatchAs(pattern) => {
                if let Some(name) = &pattern.name {
                    self.bind_identifier(active, BindingKind::MatchCapture, name);
                }
                if let Some(pattern) = &pattern.pattern {
                    self.collect_pattern(pattern, active);
                }
            }
            Pattern::MatchStar(pattern) => {
                if let Some(name) = &pattern.name {
                    self.bind_identifier(active, BindingKind::MatchCapture, name);
                }
            }
            Pattern::MatchSequence(pattern) => {
                for pattern in &pattern.patterns {
                    self.collect_pattern(pattern, active);
                }
            }
            Pattern::MatchMapping(pattern) => {
                if let Some(rest) = &pattern.rest {
                    self.bind_identifier(active, BindingKind::MatchCapture, rest);
                }
                for pattern in &pattern.patterns {
                    self.collect_pattern(pattern, active);
                }
            }
            Pattern::MatchClass(pattern) => {
                for pattern in &pattern.arguments.patterns {
                    self.collect_pattern(pattern, active);
                }
                for keyword in &pattern.arguments.keywords {
                    self.collect_pattern(&keyword.pattern, active);
                }
            }
            Pattern::MatchOr(pattern) => {
                for pattern in &pattern.patterns {
                    self.collect_pattern(pattern, active);
                }
            }
            Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
        }
    }

    fn add_name_reference(
        &mut self,
        active: &ActiveScope,
        source_name: &str,
        span: TextRange,
        role: ReferenceRole,
    ) {
        let semantic_name = self.semantic_name(active, source_name);
        let (lexical_target, lookup) = self.resolve_name(active, &semantic_name);
        self.builder.add_reference(ReferenceInput {
            module: self.module,
            source_scope: active.scope,
            source_context: active.context,
            source_spelling: source_name.to_owned(),
            semantic_name,
            lexical_target,
            lookup,
            binding_state: ReferenceBindingState::NotApplicable,
            phase: reference_phase(active, role),
            role,
            origin_domain: OriginDomain::Production,
            span,
        });
    }

    fn resolve_name(
        &mut self,
        active: &ActiveScope,
        name: &str,
    ) -> (Resolution<SymbolId>, LookupSemantics) {
        if let Some(reason) = self.invalid_declaration_reason(active, name) {
            return (Resolution::Unresolved(reason), LookupSemantics::Direct);
        }

        if active.declarations.is_global(name) {
            let symbol = self.builder.symbol(self.module, active.module_scope, name);
            return (
                Resolution::Resolved(symbol),
                LookupSemantics::GlobalThenBuiltin {
                    global_symbol: symbol,
                },
            );
        }

        if active.declarations.is_nonlocal(name) {
            if let Some(symbol) = self.find_enclosing_function_symbol(active, name) {
                return (Resolution::Resolved(symbol), LookupSemantics::Direct);
            }
            return (
                Resolution::Unresolved(UnresolvedReason::MissingNonlocalBinding),
                LookupSemantics::Direct,
            );
        }

        match active.kind {
            ScopeKind::Module => {
                let symbol = self.builder.symbol(self.module, active.module_scope, name);
                (
                    Resolution::Resolved(symbol),
                    LookupSemantics::GlobalThenBuiltin {
                        global_symbol: symbol,
                    },
                )
            }
            ScopeKind::Class => {
                if !active.declarations.locals.contains(name) {
                    if let Some(symbol) = self.find_enclosing_function_symbol(active, name) {
                        return (Resolution::Resolved(symbol), LookupSemantics::Direct);
                    }
                }

                let class_symbol = self.builder.symbol(self.module, active.scope, name);
                let global_symbol = self.builder.symbol(self.module, active.module_scope, name);
                (
                    Resolution::Resolved(class_symbol),
                    LookupSemantics::ClassLocalThenGlobalThenBuiltin {
                        class_symbol,
                        global_symbol,
                    },
                )
            }
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension => {
                if active.declarations.locals.contains(name) {
                    let symbol = self.builder.symbol(self.module, active.scope, name);
                    return (Resolution::Resolved(symbol), LookupSemantics::Direct);
                }
                if let Some(symbol) = self.find_enclosing_function_symbol(active, name) {
                    return (Resolution::Resolved(symbol), LookupSemantics::Direct);
                }
                let symbol = self.builder.symbol(self.module, active.module_scope, name);
                (
                    Resolution::Resolved(symbol),
                    LookupSemantics::GlobalThenBuiltin {
                        global_symbol: symbol,
                    },
                )
            }
        }
    }

    fn find_enclosing_function_symbol(
        &mut self,
        active: &ActiveScope,
        name: &str,
    ) -> Option<SymbolId> {
        self.find_enclosing_function_scope(active, name)
            .map(|scope| self.builder.symbol(self.module, scope, name))
    }

    fn find_enclosing_function_scope(&self, active: &ActiveScope, name: &str) -> Option<ScopeId> {
        let skip_current = usize::from(matches!(
            active.kind,
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
        ));
        for frame in active.lexical_frames.iter().rev().skip(skip_current) {
            if matches!(
                frame.kind,
                ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
            ) && frame.declarations.locals.contains(name)
            {
                return Some(frame.scope);
            }
        }
        None
    }

    fn validate_declarations(&mut self, active: &ActiveScope) {
        for name in active
            .declarations
            .globals
            .keys()
            .filter(|name| active.declarations.nonlocals.contains_key(*name))
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "CULL_P1100",
                    format!("name `{name}` declared both global and nonlocal"),
                )
                .with_path(self.display_path.to_owned())
                .with_range(active.declarations.globals[name]),
            );
        }

        for name in active
            .declarations
            .parameters
            .iter()
            .filter(|name| active.declarations.globals.contains_key(*name))
            .chain(
                active
                    .declarations
                    .parameters
                    .iter()
                    .filter(|name| active.declarations.nonlocals.contains_key(*name)),
            )
        {
            self.diagnostics.push(
                Diagnostic::error(
                    "CULL_P1101",
                    format!("parameter `{name}` conflicts with global or nonlocal declaration"),
                )
                .with_path(self.display_path.to_owned()),
            );
        }

        for name in active.declarations.nonlocals.keys() {
            if self.find_enclosing_function_symbol(active, name).is_none() {
                self.diagnostics.push(
                    Diagnostic::error(
                        "CULL_P1102",
                        format!("nonlocal declaration for `{name}` has no enclosing binding"),
                    )
                    .with_path(self.display_path.to_owned()),
                );
            }
        }

        for (name, declaration_range) in active
            .declarations
            .globals
            .iter()
            .chain(active.declarations.nonlocals.iter())
        {
            if active
                .declarations
                .first_loads
                .get(name)
                .is_some_and(|range| range.start < declaration_range.start)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "CULL_P1103",
                        format!("name `{name}` is used before its global or nonlocal declaration"),
                    )
                    .with_path(self.display_path.to_owned())
                    .with_range(*declaration_range),
                );
            }
            if active
                .declarations
                .first_bindings
                .get(name)
                .is_some_and(|range| range.start < declaration_range.start)
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        "CULL_P1104",
                        format!(
                            "name `{name}` is assigned before its global or nonlocal declaration"
                        ),
                    )
                    .with_path(self.display_path.to_owned())
                    .with_range(*declaration_range),
                );
            }
        }
    }

    fn predeclare(&mut self, scope: ScopeId, declarations: &BlockDeclarations) {
        for name in &declarations.locals {
            self.builder.symbol(self.module, scope, name);
        }
    }

    fn bind_identifier(
        &mut self,
        active: &ActiveScope,
        kind: BindingKind,
        identifier: &Identifier,
    ) -> BindingId {
        let semantic_name = self.semantic_name(active, identifier.id.as_str());
        self.bind_name(
            active,
            kind,
            semantic_name,
            to_range(identifier.range),
            to_range(identifier.range),
        )
    }

    fn bind_name(
        &mut self,
        active: &ActiveScope,
        kind: BindingKind,
        name: String,
        range: TextRange,
        name_range: TextRange,
    ) -> BindingId {
        let scope = self.binding_scope(active, &name);
        let symbol = self.builder.symbol(self.module, scope, &name);
        self.builder.add_binding(BindingInput {
            module: self.module,
            scope,
            symbol,
            kind,
            name,
            range,
            name_range,
        })
    }

    fn semantic_name(&self, active: &ActiveScope, name: &str) -> String {
        mangle_private_name(name, active.class_mangle.as_deref())
    }

    fn binding_scope(&self, active: &ActiveScope, name: &str) -> ScopeId {
        if self.invalid_declaration_reason(active, name).is_some() {
            return active.scope;
        }

        if active.declarations.is_global(name) {
            return active.module_scope;
        }

        if active.declarations.is_nonlocal(name) {
            return self
                .find_enclosing_function_scope(active, name)
                .unwrap_or(active.scope);
        }

        active.scope
    }

    fn invalid_declaration_reason(
        &self,
        active: &ActiveScope,
        name: &str,
    ) -> Option<UnresolvedReason> {
        let global_range = active.declarations.globals.get(name);
        let nonlocal_range = active.declarations.nonlocals.get(name);

        if global_range.is_some() && nonlocal_range.is_some() {
            return Some(UnresolvedReason::ConflictingDeclaration);
        }

        if let Some(range) = global_range {
            if active.declarations.parameters.contains(name)
                || declaration_follows_use_or_binding(&active.declarations, name, *range)
            {
                return Some(UnresolvedReason::InvalidGlobalDeclaration);
            }
        }

        if let Some(range) = nonlocal_range {
            if active.declarations.parameters.contains(name)
                || declaration_follows_use_or_binding(&active.declarations, name, *range)
            {
                return Some(UnresolvedReason::InvalidNonlocalDeclaration);
            }
        }

        None
    }
}

fn declaration_follows_use_or_binding(
    declarations: &BlockDeclarations,
    name: &str,
    declaration_range: TextRange,
) -> bool {
    declarations
        .first_loads
        .get(name)
        .is_some_and(|range| range.start < declaration_range.start)
        || declarations
            .first_bindings
            .get(name)
            .is_some_and(|range| range.start < declaration_range.start)
}

fn reference_phase(active: &ActiveScope, role: ReferenceRole) -> ReferencePhase {
    match role {
        ReferenceRole::Decorator
        | ReferenceRole::DefaultValue
        | ReferenceRole::BaseClass
        | ReferenceRole::ClassKeyword => ReferencePhase::DefinitionTime,
        ReferenceRole::ComprehensionIterable => {
            if matches!(
                active.kind,
                ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
            ) {
                ReferencePhase::BodyRuntime
            } else {
                ReferencePhase::DefinitionTime
            }
        }
        ReferenceRole::Value => {
            if matches!(
                active.kind,
                ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
            ) {
                ReferencePhase::BodyRuntime
            } else {
                ReferencePhase::DefinitionTime
            }
        }
    }
}

fn collect_function_declarations(
    function: &StmtFunctionDef,
    class_mangle: Option<&str>,
) -> BlockDeclarations {
    let mut declarations = collect_body_declarations(&function.body, class_mangle);
    for parameter in function.parameters.iter_source_order() {
        let name = mangle_private_name(parameter.name().id.as_str(), class_mangle);
        declarations.record_parameter(name, to_range(parameter.name().range));
    }
    declarations.finish();
    declarations
}

fn collect_lambda_declarations(
    lambda: &ruff_python_ast::ExprLambda,
    class_mangle: Option<&str>,
) -> BlockDeclarations {
    let mut declarations = BlockDeclarations::default();
    if let Some(parameters) = &lambda.parameters {
        for parameter in parameters.iter_source_order() {
            let name = mangle_private_name(parameter.name().id.as_str(), class_mangle);
            declarations.record_parameter(name, to_range(parameter.name().range));
        }
    }
    declarations.finish();
    declarations
}

fn collect_comprehension_declarations(
    generators: &[ruff_python_ast::Comprehension],
    class_mangle: Option<&str>,
) -> BlockDeclarations {
    let mut declarations = BlockDeclarations::default();
    for generator in generators {
        collect_target_declarations(&generator.target, class_mangle, &mut declarations);
    }
    declarations.finish();
    declarations
}

fn collect_body_declarations(statements: &[Stmt], class_mangle: Option<&str>) -> BlockDeclarations {
    let mut declarations = BlockDeclarations::default();
    for statement in statements {
        collect_statement_declarations(statement, class_mangle, &mut declarations);
    }
    declarations.finish();
    declarations
}

fn collect_statement_declarations(
    statement: &Stmt,
    class_mangle: Option<&str>,
    declarations: &mut BlockDeclarations,
) {
    match statement {
        Stmt::FunctionDef(function) => {
            for decorator in &function.decorator_list {
                collect_expr_declarations(&decorator.expression, class_mangle, declarations);
            }
            for parameter in function.parameters.iter_source_order() {
                if let Some(default) = parameter.default() {
                    collect_expr_declarations(default, class_mangle, declarations);
                }
            }
            declarations.record_binding(
                mangle_private_name(function.name.id.as_str(), class_mangle),
                to_range(function.name.range),
            );
        }
        Stmt::ClassDef(class) => {
            for decorator in &class.decorator_list {
                collect_expr_declarations(&decorator.expression, class_mangle, declarations);
            }
            if let Some(arguments) = &class.arguments {
                for base in &arguments.args {
                    collect_expr_declarations(base, class_mangle, declarations);
                }
                for keyword in &arguments.keywords {
                    collect_expr_declarations(&keyword.value, class_mangle, declarations);
                }
            }
            declarations.record_binding(
                mangle_private_name(class.name.id.as_str(), class_mangle),
                to_range(class.name.range),
            );
        }
        Stmt::Assign(assign) => {
            for target in &assign.targets {
                collect_target_declarations(target, class_mangle, declarations);
            }
            collect_expr_declarations(&assign.value, class_mangle, declarations);
        }
        Stmt::AnnAssign(assign) => {
            collect_target_declarations(&assign.target, class_mangle, declarations);
            if let Some(value) = &assign.value {
                collect_expr_declarations(value, class_mangle, declarations);
            }
        }
        Stmt::AugAssign(assign) => {
            collect_target_declarations(&assign.target, class_mangle, declarations);
            collect_expr_declarations(&assign.value, class_mangle, declarations);
        }
        Stmt::Delete(delete) => {
            for target in &delete.targets {
                collect_target_declarations(target, class_mangle, declarations);
            }
        }
        Stmt::TypeAlias(alias) => {
            collect_target_declarations(&alias.name, class_mangle, declarations);
        }
        Stmt::For(for_stmt) => {
            collect_expr_declarations(&for_stmt.iter, class_mangle, declarations);
            collect_target_declarations(&for_stmt.target, class_mangle, declarations);
            for statement in &for_stmt.body {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
            for statement in &for_stmt.orelse {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
        }
        Stmt::While(while_stmt) => {
            collect_expr_declarations(&while_stmt.test, class_mangle, declarations);
            for statement in &while_stmt.body {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
            for statement in &while_stmt.orelse {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
        }
        Stmt::If(if_stmt) => {
            collect_expr_declarations(&if_stmt.test, class_mangle, declarations);
            for statement in &if_stmt.body {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
            for clause in &if_stmt.elif_else_clauses {
                if let Some(test) = &clause.test {
                    collect_expr_declarations(test, class_mangle, declarations);
                }
                for statement in &clause.body {
                    collect_statement_declarations(statement, class_mangle, declarations);
                }
            }
        }
        Stmt::With(with_stmt) => {
            for item in &with_stmt.items {
                collect_expr_declarations(&item.context_expr, class_mangle, declarations);
                if let Some(target) = &item.optional_vars {
                    collect_target_declarations(target, class_mangle, declarations);
                }
            }
            for statement in &with_stmt.body {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
        }
        Stmt::Try(try_stmt) => {
            for statement in &try_stmt.body {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
            for handler in &try_stmt.handlers {
                let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                if let Some(type_) = &handler.type_ {
                    collect_expr_declarations(type_, class_mangle, declarations);
                }
                if let Some(name) = &handler.name {
                    declarations.record_binding(
                        mangle_private_name(name.id.as_str(), class_mangle),
                        to_range(name.range),
                    );
                }
                for statement in &handler.body {
                    collect_statement_declarations(statement, class_mangle, declarations);
                }
            }
            for statement in &try_stmt.orelse {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
            for statement in &try_stmt.finalbody {
                collect_statement_declarations(statement, class_mangle, declarations);
            }
        }
        Stmt::Match(match_stmt) => {
            collect_expr_declarations(&match_stmt.subject, class_mangle, declarations);
            for case in &match_stmt.cases {
                collect_pattern_declarations(&case.pattern, class_mangle, declarations);
                if let Some(guard) = &case.guard {
                    collect_expr_declarations(guard, class_mangle, declarations);
                }
                for statement in &case.body {
                    collect_statement_declarations(statement, class_mangle, declarations);
                }
            }
        }
        Stmt::Import(import) => {
            for alias in &import.names {
                let name = alias
                    .asname
                    .as_ref()
                    .map(|name| name.id.to_string())
                    .unwrap_or_else(|| import_root_name(alias.name.id.as_str()));
                let name_range = alias
                    .asname
                    .as_ref()
                    .map(|name| to_range(name.range))
                    .unwrap_or_else(|| to_range(alias.name.range));
                declarations.record_binding(mangle_private_name(&name, class_mangle), name_range);
            }
        }
        Stmt::ImportFrom(import) => {
            for alias in &import.names {
                if alias.name.id.as_str() == "*" {
                    continue;
                }
                let name = alias
                    .asname
                    .as_ref()
                    .map(|name| name.id.as_str())
                    .unwrap_or(alias.name.id.as_str());
                let name_range = alias
                    .asname
                    .as_ref()
                    .map(|name| to_range(name.range))
                    .unwrap_or_else(|| to_range(alias.name.range));
                declarations.record_binding(mangle_private_name(name, class_mangle), name_range);
            }
        }
        Stmt::Global(global) => {
            for name in &global.names {
                declarations.record_global(
                    mangle_private_name(name.id.as_str(), class_mangle),
                    to_range(name.range),
                );
            }
        }
        Stmt::Nonlocal(nonlocal) => {
            for name in &nonlocal.names {
                declarations.record_nonlocal(
                    mangle_private_name(name.id.as_str(), class_mangle),
                    to_range(name.range),
                );
            }
        }
        Stmt::Return(return_stmt) => {
            if let Some(value) = &return_stmt.value {
                collect_expr_declarations(value, class_mangle, declarations);
            }
        }
        Stmt::Expr(expr) => collect_expr_declarations(&expr.value, class_mangle, declarations),
        Stmt::Assert(assert_stmt) => {
            collect_expr_declarations(&assert_stmt.test, class_mangle, declarations);
            if let Some(msg) = &assert_stmt.msg {
                collect_expr_declarations(msg, class_mangle, declarations);
            }
        }
        Stmt::Raise(raise) => {
            if let Some(exc) = &raise.exc {
                collect_expr_declarations(exc, class_mangle, declarations);
            }
            if let Some(cause) = &raise.cause {
                collect_expr_declarations(cause, class_mangle, declarations);
            }
        }
        Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::IpyEscapeCommand(_) => {}
    }
}

fn collect_expr_declarations(
    expression: &Expr,
    class_mangle: Option<&str>,
    declarations: &mut BlockDeclarations,
) {
    match expression {
        Expr::Named(named) => {
            collect_expr_declarations(&named.value, class_mangle, declarations);
            collect_target_declarations(&named.target, class_mangle, declarations);
        }
        Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
            declarations.record_load(
                mangle_private_name(name.id.as_str(), class_mangle),
                to_range(name.range),
            );
        }
        Expr::BoolOp(expr) => {
            for value in &expr.values {
                collect_expr_declarations(value, class_mangle, declarations);
            }
        }
        Expr::BinOp(expr) => {
            collect_expr_declarations(&expr.left, class_mangle, declarations);
            collect_expr_declarations(&expr.right, class_mangle, declarations);
        }
        Expr::UnaryOp(expr) => collect_expr_declarations(&expr.operand, class_mangle, declarations),
        Expr::If(expr) => {
            collect_expr_declarations(&expr.test, class_mangle, declarations);
            collect_expr_declarations(&expr.body, class_mangle, declarations);
            collect_expr_declarations(&expr.orelse, class_mangle, declarations);
        }
        Expr::Dict(expr) => {
            for item in &expr.items {
                if let Some(key) = &item.key {
                    collect_expr_declarations(key, class_mangle, declarations);
                }
                collect_expr_declarations(&item.value, class_mangle, declarations);
            }
        }
        Expr::Set(expr) => {
            for element in &expr.elts {
                collect_expr_declarations(element, class_mangle, declarations);
            }
        }
        Expr::Await(expr) => collect_expr_declarations(&expr.value, class_mangle, declarations),
        Expr::Yield(expr) => {
            if let Some(value) = &expr.value {
                collect_expr_declarations(value, class_mangle, declarations);
            }
        }
        Expr::YieldFrom(expr) => collect_expr_declarations(&expr.value, class_mangle, declarations),
        Expr::Compare(expr) => {
            collect_expr_declarations(&expr.left, class_mangle, declarations);
            for comparator in &expr.comparators {
                collect_expr_declarations(comparator, class_mangle, declarations);
            }
        }
        Expr::Call(expr) => {
            collect_expr_declarations(&expr.func, class_mangle, declarations);
            for arg in &expr.arguments.args {
                collect_expr_declarations(arg, class_mangle, declarations);
            }
            for keyword in &expr.arguments.keywords {
                collect_expr_declarations(&keyword.value, class_mangle, declarations);
            }
        }
        Expr::Attribute(expr) => collect_expr_declarations(&expr.value, class_mangle, declarations),
        Expr::Subscript(expr) => {
            collect_expr_declarations(&expr.value, class_mangle, declarations);
            collect_expr_declarations(&expr.slice, class_mangle, declarations);
        }
        Expr::Starred(expr) => collect_expr_declarations(&expr.value, class_mangle, declarations),
        Expr::List(expr) => {
            for element in &expr.elts {
                collect_expr_declarations(element, class_mangle, declarations);
            }
        }
        Expr::Tuple(expr) => {
            for element in &expr.elts {
                collect_expr_declarations(element, class_mangle, declarations);
            }
        }
        Expr::Slice(expr) => {
            if let Some(lower) = &expr.lower {
                collect_expr_declarations(lower, class_mangle, declarations);
            }
            if let Some(upper) = &expr.upper {
                collect_expr_declarations(upper, class_mangle, declarations);
            }
            if let Some(step) = &expr.step {
                collect_expr_declarations(step, class_mangle, declarations);
            }
        }
        Expr::FString(expr) => {
            collect_interpolated_string_declarations(
                expr.value.as_slice(),
                class_mangle,
                declarations,
            );
        }
        Expr::TString(expr) => {
            for string in expr.value.as_slice() {
                collect_interpolated_element_declarations(
                    &string.elements,
                    class_mangle,
                    declarations,
                );
            }
        }
        Expr::Lambda(lambda) => {
            if let Some(parameters) = &lambda.parameters {
                for parameter in parameters.iter_source_order() {
                    if let Some(default) = parameter.default() {
                        collect_expr_declarations(default, class_mangle, declarations);
                    }
                }
            }
        }
        Expr::ListComp(_)
        | Expr::SetComp(_)
        | Expr::DictComp(_)
        | Expr::Generator(_)
        | Expr::Name(_)
        | Expr::StringLiteral(_)
        | Expr::BytesLiteral(_)
        | Expr::NumberLiteral(_)
        | Expr::BooleanLiteral(_)
        | Expr::NoneLiteral(_)
        | Expr::EllipsisLiteral(_)
        | Expr::IpyEscapeCommand(_) => {}
    }
}

fn collect_interpolated_string_declarations(
    parts: &[FStringPart],
    class_mangle: Option<&str>,
    declarations: &mut BlockDeclarations,
) {
    for part in parts {
        if let FStringPart::FString(string) = part {
            collect_interpolated_element_declarations(&string.elements, class_mangle, declarations);
        }
    }
}

fn collect_interpolated_element_declarations(
    elements: &ruff_python_ast::InterpolatedStringElements,
    class_mangle: Option<&str>,
    declarations: &mut BlockDeclarations,
) {
    for element in elements {
        if let InterpolatedStringElement::Interpolation(interpolation) = element {
            collect_expr_declarations(&interpolation.expression, class_mangle, declarations);
            if let Some(format_spec) = &interpolation.format_spec {
                collect_interpolated_element_declarations(
                    &format_spec.elements,
                    class_mangle,
                    declarations,
                );
            }
        }
    }
}

fn collect_target_declarations(
    target: &Expr,
    class_mangle: Option<&str>,
    declarations: &mut BlockDeclarations,
) {
    match target {
        Expr::Name(name) => {
            declarations.record_binding(
                mangle_private_name(name.id.as_str(), class_mangle),
                to_range(name.range),
            );
        }
        Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                collect_target_declarations(element, class_mangle, declarations);
            }
        }
        Expr::List(list) => {
            for element in &list.elts {
                collect_target_declarations(element, class_mangle, declarations);
            }
        }
        Expr::Starred(starred) => {
            collect_target_declarations(&starred.value, class_mangle, declarations);
        }
        _ => {}
    }
}

fn collect_pattern_declarations(
    pattern: &Pattern,
    class_mangle: Option<&str>,
    declarations: &mut BlockDeclarations,
) {
    match pattern {
        Pattern::MatchAs(pattern) => {
            if let Some(name) = &pattern.name {
                declarations.record_binding(
                    mangle_private_name(name.id.as_str(), class_mangle),
                    to_range(name.range),
                );
            }
            if let Some(pattern) = &pattern.pattern {
                collect_pattern_declarations(pattern, class_mangle, declarations);
            }
        }
        Pattern::MatchStar(pattern) => {
            if let Some(name) = &pattern.name {
                declarations.record_binding(
                    mangle_private_name(name.id.as_str(), class_mangle),
                    to_range(name.range),
                );
            }
        }
        Pattern::MatchSequence(pattern) => {
            for pattern in &pattern.patterns {
                collect_pattern_declarations(pattern, class_mangle, declarations);
            }
        }
        Pattern::MatchMapping(pattern) => {
            if let Some(rest) = &pattern.rest {
                declarations.record_binding(
                    mangle_private_name(rest.id.as_str(), class_mangle),
                    to_range(rest.range),
                );
            }
            for pattern in &pattern.patterns {
                collect_pattern_declarations(pattern, class_mangle, declarations);
            }
        }
        Pattern::MatchClass(pattern) => {
            for pattern in &pattern.arguments.patterns {
                collect_pattern_declarations(pattern, class_mangle, declarations);
            }
            for keyword in &pattern.arguments.keywords {
                collect_pattern_declarations(&keyword.pattern, class_mangle, declarations);
            }
        }
        Pattern::MatchOr(pattern) => {
            for pattern in &pattern.patterns {
                collect_pattern_declarations(pattern, class_mangle, declarations);
            }
        }
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
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

fn mangle_class_name(class_name: &str) -> String {
    class_name.trim_start_matches('_').to_owned()
}

fn mangle_private_name(name: &str, class_name: Option<&str>) -> String {
    let Some(class_name) = class_name.filter(|class_name| !class_name.is_empty()) else {
        return name.to_owned();
    };
    if name.starts_with("__") && !name.ends_with("__") && !name.contains('.') {
        format!("_{class_name}{name}")
    } else {
        name.to_owned()
    }
}
