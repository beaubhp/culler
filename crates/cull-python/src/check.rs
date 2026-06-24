use std::collections::{BTreeMap, BTreeSet};

use crate::{
    analysis::{ParsedProjectModule, SemanticProjectData},
    module_namespace::{
        has_module_getattr, is_package_module, LocalModuleResolution, ModuleNamespaceIndex,
    },
    ruff_frontend::to_range,
};
use cull_core::{
    BindingFact, BindingId, BindingKind, CheckOutput, CheckSummary, DefId, DefinitionEffectKind,
    DefinitionKind, DefinitionSurface, Diagnostic, DiagnosticSeverity, Finding, FindingConfidence,
    FindingDefinition, FindingExportKind, FindingModeEffect, FindingOriginSummary,
    FindingReachability, FindingReachabilityStatus, FindingReferenceKind, FindingRemovalRisk,
    FindingRule, FindingType, FindingUncertainty, FindingUncertaintyKind, LocalReachability,
    ModuleId, ProjectMode, ReferenceFact, ReferencePhase, RemovalRisk, SemanticDefinition,
    SemanticGraph, TextRange,
};
use ruff_python_ast::{
    Arguments, Expr, ExprAttribute, ExprCall, ExprContext, ExprName, ModModule, Stmt, StmtAssign,
    StmtAugAssign, StmtDelete, StmtImport, StmtImportFrom,
};

pub(crate) fn analyze_part2(
    data: SemanticProjectData,
    mode_override: Option<ProjectMode>,
) -> CheckOutput {
    let mode = mode_override.unwrap_or(data.project.mode);
    let mut diagnostics = data.diagnostics;
    let mut namespace = ModuleNamespaceIndex::build(&data.project);

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        return CheckOutput {
            schema_version: 1,
            target_python: data.project.target_python,
            project_root: crate::paths::slash_path(&data.project.project_root),
            source_roots: data.project.source_root_output(),
            mode,
            findings: Vec::new(),
            summary: CheckSummary::default(),
            diagnostics,
        };
    }

    let facts = ProjectFacts::new(&data.graph, &data.parsed_modules);
    let mut resolver = ProjectResolver::new(
        &data.graph,
        &data.parsed_modules,
        &facts,
        &mut namespace,
        mode,
    );
    resolver.collect_operations();
    resolver.solve();
    let findings = build_findings(&data.graph, &data.parsed_modules, &facts, &resolver, mode);
    let summary = summarize_findings(&findings);
    diagnostics.sort_by(compare_diagnostics);

    CheckOutput {
        schema_version: 1,
        target_python: data.project.target_python,
        project_root: crate::paths::slash_path(&data.project.project_root),
        source_roots: data.project.source_root_output(),
        mode,
        findings,
        summary,
        diagnostics,
    }
}

struct ProjectFacts<'a> {
    modules: BTreeMap<ModuleId, &'a ParsedProjectModule>,
    module_names: BTreeMap<ModuleId, String>,
    module_scopes: BTreeMap<ModuleId, cull_core::ScopeId>,
    definitions_by_binding: BTreeMap<BindingId, DefId>,
    bindings: BTreeMap<BindingId, &'a BindingFact>,
    bindings_by_module_name_range_kind: BTreeMap<(ModuleId, u32, u32, BindingKind), Vec<BindingId>>,
    references_by_module_range_name: BTreeMap<(ModuleId, u32, u32, String), &'a ReferenceFact>,
    effect_sets: BTreeMap<cull_core::DefinitionEffectSetId, Vec<DefinitionEffectKind>>,
}

impl<'a> ProjectFacts<'a> {
    fn new(graph: &'a SemanticGraph, parsed_modules: &'a [ParsedProjectModule]) -> Self {
        let modules = parsed_modules
            .iter()
            .map(|module| (module.module.id, module))
            .collect::<BTreeMap<_, _>>();
        let module_names = graph
            .modules
            .iter()
            .map(|module| (module.id, module.name.clone()))
            .collect::<BTreeMap<_, _>>();
        let module_scopes = graph
            .modules
            .iter()
            .map(|module| (module.id, module.scope))
            .collect::<BTreeMap<_, _>>();

        let mut definitions_by_binding = BTreeMap::new();
        for definition in &graph.definitions {
            definitions_by_binding.insert(definition.binding, definition.id);
        }

        let mut bindings = BTreeMap::new();
        let mut bindings_by_module_name_range_kind = BTreeMap::new();
        for binding in &graph.bindings {
            bindings.insert(binding.id, binding);
            bindings_by_module_name_range_kind
                .entry((
                    binding.module,
                    binding.name_range.start,
                    binding.name_range.end,
                    binding.kind,
                ))
                .or_insert_with(Vec::new)
                .push(binding.id);
        }

        let references_by_module_range_name = graph
            .references
            .iter()
            .map(|reference| {
                (
                    (
                        reference.module,
                        reference.span.start,
                        reference.span.end,
                        reference.source_spelling.clone(),
                    ),
                    reference,
                )
            })
            .collect::<BTreeMap<_, _>>();

        let effect_sets = graph
            .definition_effect_sets
            .iter()
            .map(|set| (set.id, set.effects.clone()))
            .collect::<BTreeMap<_, _>>();

        Self {
            modules,
            module_names,
            module_scopes,
            definitions_by_binding,
            bindings,
            bindings_by_module_name_range_kind,
            references_by_module_range_name,
            effect_sets,
        }
    }

    fn binding_for_alias(
        &self,
        module: ModuleId,
        range: TextRange,
        kind: BindingKind,
    ) -> Option<BindingId> {
        self.bindings_by_module_name_range_kind
            .get(&(module, range.start, range.end, kind))
            .and_then(|bindings| bindings.first().copied())
    }

    fn reference_for_name(&self, module: ModuleId, name: &ExprName) -> Option<&'a ReferenceFact> {
        let range = to_range(name.range);
        self.references_by_module_range_name
            .get(&(module, range.start, range.end, name.id.to_string()))
            .copied()
    }

    fn module_name(&self, module: ModuleId) -> String {
        self.module_names
            .get(&module)
            .cloned()
            .unwrap_or_else(|| format!("<module:{}>", module.as_u32()))
    }

    fn module_source(&self, module: ModuleId) -> Option<&'a ParsedProjectModule> {
        self.modules.get(&module).copied()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ValueSet {
    modules: BTreeSet<ModuleId>,
    definitions: BTreeSet<DefId>,
    external: bool,
    importlib_module: bool,
    importlib_import_module: bool,
    uncertainties: BTreeSet<Part2Uncertainty>,
}

impl ValueSet {
    fn module(module: ModuleId) -> Self {
        Self {
            modules: BTreeSet::from([module]),
            ..Self::default()
        }
    }

    fn definition(definition: DefId) -> Self {
        Self {
            definitions: BTreeSet::from([definition]),
            ..Self::default()
        }
    }

    fn external(kind: Part2Uncertainty) -> Self {
        Self {
            external: true,
            uncertainties: BTreeSet::from([kind]),
            ..Self::default()
        }
    }

    fn importlib_module() -> Self {
        Self {
            external: true,
            importlib_module: true,
            ..Self::default()
        }
    }

    fn importlib_import_module() -> Self {
        Self {
            external: true,
            importlib_import_module: true,
            ..Self::default()
        }
    }

    fn union_with(&mut self, other: Self) -> bool {
        let before = self.clone();
        self.modules.extend(other.modules);
        self.definitions.extend(other.definitions);
        self.external |= other.external;
        self.importlib_module |= other.importlib_module;
        self.importlib_import_module |= other.importlib_import_module;
        self.uncertainties.extend(other.uncertainties);
        *self != before
    }

    fn with_uncertainty(mut self, uncertainty: Part2Uncertainty) -> Self {
        self.uncertainties.insert(uncertainty);
        self
    }

    fn is_empty(&self) -> bool {
        self.modules.is_empty()
            && self.definitions.is_empty()
            && !self.external
            && !self.importlib_module
            && !self.importlib_import_module
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum Part2Uncertainty {
    DynamicExport,
    DynamicImport,
    DynamicModuleAttribute,
    ExternalImport,
    ImportResolution,
    ModuleGetattr,
    NamespaceOrder,
    PartialInitialization,
    UnsupportedNamespace,
}

impl Part2Uncertainty {
    fn output_kind(self) -> FindingUncertaintyKind {
        match self {
            Self::DynamicExport => FindingUncertaintyKind::DynamicExport,
            Self::DynamicImport => FindingUncertaintyKind::DynamicImport,
            Self::DynamicModuleAttribute => FindingUncertaintyKind::DynamicModuleAttribute,
            Self::ExternalImport => FindingUncertaintyKind::ExternalImport,
            Self::ImportResolution => FindingUncertaintyKind::ImportResolution,
            Self::ModuleGetattr => FindingUncertaintyKind::ModuleGetattr,
            Self::NamespaceOrder => FindingUncertaintyKind::NamespaceOrder,
            Self::PartialInitialization => FindingUncertaintyKind::PartialInitialization,
            Self::UnsupportedNamespace => FindingUncertaintyKind::UnsupportedNamespace,
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::DynamicExport => "dynamic export surface may hide references",
            Self::DynamicImport => "dynamic import target is not statically known",
            Self::DynamicModuleAttribute => {
                "dynamic module attribute mutation may affect the namespace"
            }
            Self::ExternalImport => {
                "external import may reference project code through environment behavior"
            }
            Self::ImportResolution => "import resolution is unresolved or unsupported",
            Self::ModuleGetattr => "module __getattr__ may synthesize missing attributes",
            Self::NamespaceOrder => "package attribute order could not be proven",
            Self::PartialInitialization => {
                "circular import may observe a partially initialized module"
            }
            Self::UnsupportedNamespace => "namespace provider could not be fully modeled",
        }
    }
}

#[derive(Clone, Debug)]
struct CrossReference {
    definition: DefId,
}

#[derive(Clone, Debug)]
struct ExportReference {
    definition: DefId,
    public_name: String,
    kind: FindingExportKind,
    source_module: ModuleId,
}

#[derive(Clone)]
struct ImportOperation {
    module: ModuleId,
    range: TextRange,
    kind: ImportOperationKind,
    phase: ReferencePhase,
}

#[derive(Clone)]
enum ImportOperationKind {
    Import {
        requested: String,
        binding: BindingId,
        asname: bool,
    },
    ImportFrom {
        module_name: Option<String>,
        level: u32,
        name: String,
        binding: BindingId,
    },
    StarImport {
        module_name: Option<String>,
        level: u32,
    },
}

#[derive(Clone)]
struct AssignmentOperation {
    module: ModuleId,
    binding: BindingId,
    value: Expr,
}

#[derive(Clone)]
struct AttributeOperation {
    module: ModuleId,
    expression: ExprAttribute,
    kind: AttributeOperationKind,
    phase: ReferencePhase,
}

#[derive(Clone)]
struct DynamicImportOperation {
    module: ModuleId,
    call: ExprCall,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AttributeOperationKind {
    Read,
    Write,
    Delete,
}

struct ProjectResolver<'a, 'b> {
    graph: &'a SemanticGraph,
    parsed_modules: &'a [ParsedProjectModule],
    facts: &'a ProjectFacts<'a>,
    namespace: &'b mut ModuleNamespaceIndex,
    mode: ProjectMode,
    import_operations: Vec<ImportOperation>,
    assignment_operations: Vec<AssignmentOperation>,
    attribute_operations: Vec<AttributeOperation>,
    dynamic_import_operations: Vec<DynamicImportOperation>,
    star_imports: Vec<ImportOperation>,
    binding_values: BTreeMap<BindingId, ValueSet>,
    module_slots: BTreeMap<(ModuleId, String), ValueSet>,
    cross_references: Vec<CrossReference>,
    export_references: Vec<ExportReference>,
    module_uncertainty: BTreeMap<ModuleId, BTreeSet<Part2Uncertainty>>,
}

impl<'a, 'b> ProjectResolver<'a, 'b> {
    fn new(
        graph: &'a SemanticGraph,
        parsed_modules: &'a [ParsedProjectModule],
        facts: &'a ProjectFacts<'a>,
        namespace: &'b mut ModuleNamespaceIndex,
        mode: ProjectMode,
    ) -> Self {
        Self {
            graph,
            parsed_modules,
            facts,
            namespace,
            mode,
            import_operations: Vec::new(),
            assignment_operations: Vec::new(),
            attribute_operations: Vec::new(),
            dynamic_import_operations: Vec::new(),
            star_imports: Vec::new(),
            binding_values: BTreeMap::new(),
            module_slots: BTreeMap::new(),
            cross_references: Vec::new(),
            export_references: Vec::new(),
            module_uncertainty: BTreeMap::new(),
        }
    }

    fn collect_operations(&mut self) {
        for parsed in self.parsed_modules {
            self.seed_definition_slots(parsed.module.id);
            self.collect_statements(
                parsed.module.id,
                &parsed.syntax.body,
                ReferencePhase::ImportTime,
            );
            if has_module_getattr(&parsed.syntax) {
                self.module_uncertainty
                    .entry(parsed.module.id)
                    .or_default()
                    .insert(Part2Uncertainty::ModuleGetattr);
            }
        }
        self.mark_circular_import_uncertainty();
    }

    fn seed_definition_slots(&mut self, module: ModuleId) {
        for definition in self
            .graph
            .definitions
            .iter()
            .filter(|definition| definition.module == module && definition.reportable)
        {
            self.module_slots
                .entry((module, definition.name.clone()))
                .or_default()
                .union_with(ValueSet::definition(definition.id));
        }
    }

    fn collect_statements(&mut self, module: ModuleId, statements: &[Stmt], phase: ReferencePhase) {
        for statement in statements {
            match statement {
                Stmt::Import(import) => self.collect_import(module, import, phase),
                Stmt::ImportFrom(import) => self.collect_import_from(module, import, phase),
                Stmt::Assign(assign) => {
                    self.collect_assignment(module, assign);
                    self.collect_expr_operations(module, &assign.value, phase);
                    for target in &assign.targets {
                        self.collect_target_attribute_operations(
                            module,
                            target,
                            AttributeOperationKind::Write,
                            phase,
                        );
                    }
                }
                Stmt::AnnAssign(assign) => {
                    if let Some(value) = &assign.value {
                        self.collect_expr_operations(module, value, phase);
                    }
                    self.collect_target_attribute_operations(
                        module,
                        &assign.target,
                        AttributeOperationKind::Write,
                        phase,
                    );
                }
                Stmt::AugAssign(assign) => {
                    self.collect_aug_assignment(module, assign, phase);
                }
                Stmt::Delete(delete) => self.collect_delete(module, delete, phase),
                Stmt::If(if_stmt) => {
                    let child_phase = if is_type_checking_expr(&if_stmt.test) {
                        ReferencePhase::TypeOnly
                    } else {
                        phase
                    };
                    self.collect_expr_operations(module, &if_stmt.test, phase);
                    self.collect_statements(module, &if_stmt.body, child_phase);
                    for clause in &if_stmt.elif_else_clauses {
                        if let Some(test) = &clause.test {
                            self.collect_expr_operations(module, test, phase);
                        }
                        self.collect_statements(module, &clause.body, phase);
                    }
                }
                Stmt::FunctionDef(function) => {
                    for decorator in &function.decorator_list {
                        self.collect_expr_operations(module, &decorator.expression, phase);
                    }
                    self.collect_statements(module, &function.body, ReferencePhase::BodyRuntime);
                }
                Stmt::ClassDef(class) => {
                    for decorator in &class.decorator_list {
                        self.collect_expr_operations(module, &decorator.expression, phase);
                    }
                    if let Some(arguments) = &class.arguments {
                        for arg in &arguments.args {
                            self.collect_expr_operations(module, arg, phase);
                        }
                        for keyword in &arguments.keywords {
                            self.collect_expr_operations(module, &keyword.value, phase);
                        }
                    }
                    self.collect_statements(module, &class.body, ReferencePhase::ImportTime);
                }
                _ => self.collect_statement_exprs(module, statement, phase),
            }
        }
    }

    fn collect_import(&mut self, module: ModuleId, import: &StmtImport, phase: ReferencePhase) {
        for alias in &import.names {
            let name_range = alias
                .asname
                .as_ref()
                .map(|name| to_range(name.range))
                .unwrap_or_else(|| to_range(alias.name.range));
            if let Some(binding) =
                self.facts
                    .binding_for_alias(module, name_range, BindingKind::Import)
            {
                self.import_operations.push(ImportOperation {
                    module,
                    range: to_range(alias.range),
                    kind: ImportOperationKind::Import {
                        requested: alias.name.id.to_string(),
                        binding,
                        asname: alias.asname.is_some(),
                    },
                    phase,
                });
            }
        }
    }

    fn collect_import_from(
        &mut self,
        module: ModuleId,
        import: &StmtImportFrom,
        phase: ReferencePhase,
    ) {
        for alias in &import.names {
            if alias.name.id.as_str() == "*" {
                self.star_imports.push(ImportOperation {
                    module,
                    range: to_range(alias.range),
                    kind: ImportOperationKind::StarImport {
                        module_name: import.module.as_ref().map(|module| module.id.to_string()),
                        level: import.level,
                    },
                    phase,
                });
                continue;
            }
            let name_range = alias
                .asname
                .as_ref()
                .map(|name| to_range(name.range))
                .unwrap_or_else(|| to_range(alias.name.range));
            if let Some(binding) =
                self.facts
                    .binding_for_alias(module, name_range, BindingKind::ImportFrom)
            {
                self.import_operations.push(ImportOperation {
                    module,
                    range: to_range(alias.range),
                    kind: ImportOperationKind::ImportFrom {
                        module_name: import.module.as_ref().map(|module| module.id.to_string()),
                        level: import.level,
                        name: alias.name.id.to_string(),
                        binding,
                    },
                    phase,
                });
            }
        }
    }

    fn collect_assignment(&mut self, module: ModuleId, assign: &StmtAssign) {
        for target in &assign.targets {
            if let Expr::Name(name) = target {
                if !matches!(name.ctx, ExprContext::Store) {
                    continue;
                }
                let range = to_range(name.range);
                if let Some(binding) =
                    self.facts
                        .binding_for_alias(module, range, BindingKind::Assignment)
                {
                    self.assignment_operations.push(AssignmentOperation {
                        module,
                        binding,
                        value: (*assign.value).clone(),
                    });
                }
            }
        }
    }

    fn collect_aug_assignment(
        &mut self,
        module: ModuleId,
        assign: &StmtAugAssign,
        phase: ReferencePhase,
    ) {
        self.collect_expr_operations(module, &assign.value, phase);
        self.collect_target_attribute_operations(
            module,
            &assign.target,
            AttributeOperationKind::Write,
            phase,
        );
    }

    fn collect_delete(&mut self, module: ModuleId, delete: &StmtDelete, phase: ReferencePhase) {
        for target in &delete.targets {
            self.collect_target_attribute_operations(
                module,
                target,
                AttributeOperationKind::Delete,
                phase,
            );
        }
    }

    fn collect_target_attribute_operations(
        &mut self,
        module: ModuleId,
        target: &Expr,
        kind: AttributeOperationKind,
        phase: ReferencePhase,
    ) {
        match target {
            Expr::Attribute(attribute) => {
                self.attribute_operations.push(AttributeOperation {
                    module,
                    expression: attribute.clone(),
                    kind,
                    phase,
                });
                self.collect_expr_operations(module, &attribute.value, phase);
            }
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.collect_target_attribute_operations(module, element, kind, phase);
                }
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.collect_target_attribute_operations(module, element, kind, phase);
                }
            }
            _ => {}
        }
    }

    fn collect_statement_exprs(
        &mut self,
        module: ModuleId,
        statement: &Stmt,
        phase: ReferencePhase,
    ) {
        match statement {
            Stmt::Expr(expr) => self.collect_expr_operations(module, &expr.value, phase),
            Stmt::Return(stmt) => {
                if let Some(value) = &stmt.value {
                    self.collect_expr_operations(module, value, phase);
                }
            }
            Stmt::For(stmt) => {
                self.collect_expr_operations(module, &stmt.iter, phase);
                self.collect_statements(module, &stmt.body, phase);
                self.collect_statements(module, &stmt.orelse, phase);
            }
            Stmt::While(stmt) => {
                self.collect_expr_operations(module, &stmt.test, phase);
                self.collect_statements(module, &stmt.body, phase);
                self.collect_statements(module, &stmt.orelse, phase);
            }
            Stmt::With(stmt) => {
                for item in &stmt.items {
                    self.collect_expr_operations(module, &item.context_expr, phase);
                }
                self.collect_statements(module, &stmt.body, phase);
            }
            Stmt::Try(stmt) => {
                self.collect_statements(module, &stmt.body, phase);
                for handler in &stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(type_) = &handler.type_ {
                        self.collect_expr_operations(module, type_, phase);
                    }
                    self.collect_statements(module, &handler.body, phase);
                }
                self.collect_statements(module, &stmt.orelse, phase);
                self.collect_statements(module, &stmt.finalbody, phase);
            }
            Stmt::Assert(stmt) => {
                self.collect_expr_operations(module, &stmt.test, phase);
                if let Some(msg) = &stmt.msg {
                    self.collect_expr_operations(module, msg, phase);
                }
            }
            Stmt::Raise(stmt) => {
                if let Some(exc) = &stmt.exc {
                    self.collect_expr_operations(module, exc, phase);
                }
                if let Some(cause) = &stmt.cause {
                    self.collect_expr_operations(module, cause, phase);
                }
            }
            Stmt::Match(stmt) => {
                self.collect_expr_operations(module, &stmt.subject, phase);
                for case in &stmt.cases {
                    if let Some(guard) = &case.guard {
                        self.collect_expr_operations(module, guard, phase);
                    }
                    self.collect_statements(module, &case.body, phase);
                }
            }
            _ => {}
        }
    }

    fn collect_expr_operations(
        &mut self,
        module: ModuleId,
        expression: &Expr,
        phase: ReferencePhase,
    ) {
        match expression {
            Expr::Attribute(attribute) => {
                if matches!(attribute.ctx, ExprContext::Load) {
                    self.attribute_operations.push(AttributeOperation {
                        module,
                        expression: attribute.clone(),
                        kind: AttributeOperationKind::Read,
                        phase,
                    });
                }
                self.collect_expr_operations(module, &attribute.value, phase);
            }
            Expr::Call(call) => {
                self.collect_dynamic_import_call(module, call, phase);
                self.collect_expr_operations(module, &call.func, phase);
                collect_arguments(&call.arguments, |expr| {
                    self.collect_expr_operations(module, expr, phase)
                });
            }
            Expr::BoolOp(expr) => {
                for value in &expr.values {
                    self.collect_expr_operations(module, value, phase);
                }
            }
            Expr::Named(expr) => {
                self.collect_expr_operations(module, &expr.value, phase);
            }
            Expr::BinOp(expr) => {
                self.collect_expr_operations(module, &expr.left, phase);
                self.collect_expr_operations(module, &expr.right, phase);
            }
            Expr::UnaryOp(expr) => self.collect_expr_operations(module, &expr.operand, phase),
            Expr::If(expr) => {
                self.collect_expr_operations(module, &expr.test, phase);
                self.collect_expr_operations(module, &expr.body, phase);
                self.collect_expr_operations(module, &expr.orelse, phase);
            }
            Expr::Compare(expr) => {
                self.collect_expr_operations(module, &expr.left, phase);
                for comparator in &expr.comparators {
                    self.collect_expr_operations(module, comparator, phase);
                }
            }
            Expr::Subscript(expr) => {
                self.collect_expr_operations(module, &expr.value, phase);
                self.collect_expr_operations(module, &expr.slice, phase);
            }
            Expr::List(expr) => {
                for element in &expr.elts {
                    self.collect_expr_operations(module, element, phase);
                }
            }
            Expr::Tuple(expr) => {
                for element in &expr.elts {
                    self.collect_expr_operations(module, element, phase);
                }
            }
            Expr::Dict(expr) => {
                for item in &expr.items {
                    if let Some(key) = &item.key {
                        self.collect_expr_operations(module, key, phase);
                    }
                    self.collect_expr_operations(module, &item.value, phase);
                }
            }
            Expr::Set(expr) => {
                for element in &expr.elts {
                    self.collect_expr_operations(module, element, phase);
                }
            }
            Expr::Starred(expr) => self.collect_expr_operations(module, &expr.value, phase),
            Expr::Slice(expr) => {
                if let Some(lower) = &expr.lower {
                    self.collect_expr_operations(module, lower, phase);
                }
                if let Some(upper) = &expr.upper {
                    self.collect_expr_operations(module, upper, phase);
                }
                if let Some(step) = &expr.step {
                    self.collect_expr_operations(module, step, phase);
                }
            }
            _ => {}
        }
    }

    fn collect_dynamic_import_call(
        &mut self,
        module: ModuleId,
        call: &ExprCall,
        _phase: ReferencePhase,
    ) {
        self.dynamic_import_operations.push(DynamicImportOperation {
            module,
            call: call.clone(),
        });
    }

    fn mark_circular_import_uncertainty(&mut self) {
        let edges = self.local_import_edges();
        let mut cycle_nodes = BTreeSet::new();
        for start in edges.keys().copied() {
            let mut path = vec![start];
            self.collect_cycle_nodes(start, start, &edges, &mut path, &mut cycle_nodes);
        }
        for module in cycle_nodes {
            self.module_uncertainty
                .entry(module)
                .or_default()
                .insert(Part2Uncertainty::PartialInitialization);
        }
    }

    fn collect_cycle_nodes(
        &self,
        start: ModuleId,
        current: ModuleId,
        edges: &BTreeMap<ModuleId, BTreeSet<ModuleId>>,
        path: &mut Vec<ModuleId>,
        cycle_nodes: &mut BTreeSet<ModuleId>,
    ) {
        let Some(targets) = edges.get(&current) else {
            return;
        };
        for target in targets {
            if *target == start {
                cycle_nodes.extend(path.iter().copied());
                cycle_nodes.insert(start);
                continue;
            }
            if path.contains(target) {
                continue;
            }
            path.push(*target);
            self.collect_cycle_nodes(start, *target, edges, path, cycle_nodes);
            path.pop();
        }
    }

    fn local_import_edges(&mut self) -> BTreeMap<ModuleId, BTreeSet<ModuleId>> {
        let mut edges: BTreeMap<ModuleId, BTreeSet<ModuleId>> = BTreeMap::new();
        for operation in self.import_operations.clone() {
            match operation.kind {
                ImportOperationKind::Import { requested, .. } => {
                    if let LocalModuleResolution::Module(target) =
                        self.namespace.resolve_absolute(&requested)
                    {
                        edges.entry(operation.module).or_default().insert(target);
                    }
                }
                ImportOperationKind::ImportFrom {
                    module_name,
                    level,
                    name,
                    ..
                } => {
                    if let Some(base_name) = self.namespace.relative_module_name(
                        operation.module,
                        level,
                        module_name.as_deref(),
                    ) {
                        if let LocalModuleResolution::Module(target) =
                            self.namespace.resolve_absolute(&base_name)
                        {
                            edges.entry(operation.module).or_default().insert(target);
                        }
                        let submodule = format!("{base_name}.{name}");
                        if let LocalModuleResolution::Module(target) =
                            self.namespace.resolve_absolute(&submodule)
                        {
                            edges.entry(operation.module).or_default().insert(target);
                        }
                    }
                }
                ImportOperationKind::StarImport { .. } => {}
            }
        }
        for operation in self.star_imports.clone() {
            if let ImportOperationKind::StarImport { module_name, level } = operation.kind {
                if let Some(imported_module_name) = self.namespace.relative_module_name(
                    operation.module,
                    level,
                    module_name.as_deref(),
                ) {
                    if let LocalModuleResolution::Module(target) =
                        self.namespace.resolve_absolute(&imported_module_name)
                    {
                        edges.entry(operation.module).or_default().insert(target);
                    }
                }
            }
        }
        edges
    }

    fn solve(&mut self) {
        let mut changed = true;
        let mut iterations = 0usize;
        while changed && iterations < 64 {
            iterations += 1;
            changed = false;
            for operation in self.import_operations.clone() {
                changed |= self.apply_import_operation(operation);
            }
            for assignment in self.assignment_operations.clone() {
                let value = self.resolve_expr_value(assignment.module, &assignment.value);
                changed |= self.record_binding_value(assignment.binding, value);
            }
            for operation in self.dynamic_import_operations.clone() {
                changed |= self.apply_dynamic_import_operation(operation);
            }
            for operation in self.attribute_operations.clone() {
                changed |= self.apply_attribute_operation(operation);
            }
            for star in self.star_imports.clone() {
                changed |= self.apply_star_import(star);
            }
            changed |= self.apply_exports();
        }
        if iterations >= 64 {
            for parsed in self.parsed_modules {
                self.module_uncertainty
                    .entry(parsed.module.id)
                    .or_default()
                    .insert(Part2Uncertainty::UnsupportedNamespace);
            }
        }
    }

    fn apply_import_operation(&mut self, operation: ImportOperation) -> bool {
        let ImportOperation {
            module,
            range,
            kind,
            phase,
        } = operation;
        match kind {
            ImportOperationKind::Import {
                requested,
                binding,
                asname,
            } => {
                let mut changed = false;
                let target_name = if asname {
                    requested.clone()
                } else {
                    requested
                        .split('.')
                        .next()
                        .map(str::to_owned)
                        .unwrap_or_else(|| requested.clone())
                };
                match self.namespace.resolve_absolute(&target_name) {
                    LocalModuleResolution::Module(module) => {
                        changed |= self.record_binding_value(binding, ValueSet::module(module));
                    }
                    LocalModuleResolution::Namespace(_) => {
                        changed |= self.record_binding_value(
                            binding,
                            ValueSet::default()
                                .with_uncertainty(Part2Uncertainty::UnsupportedNamespace),
                        );
                    }
                    LocalModuleResolution::External => {
                        let value = if requested == "importlib" {
                            ValueSet::importlib_module()
                        } else {
                            ValueSet::external(Part2Uncertainty::ExternalImport)
                        };
                        changed |= self.record_binding_value(binding, value);
                    }
                    LocalModuleResolution::Unsupported(_) => {
                        changed |= self.record_binding_value(
                            binding,
                            ValueSet::default()
                                .with_uncertainty(Part2Uncertainty::ImportResolution),
                        );
                    }
                }
                if let LocalModuleResolution::Module(full_module) =
                    self.namespace.resolve_absolute(&requested)
                {
                    changed |= self.add_parent_package_attributes(&requested, full_module);
                }
                changed
            }
            ImportOperationKind::ImportFrom {
                module_name,
                level,
                name,
                binding,
            } => {
                let target = self.resolve_import_from(module, module_name.as_deref(), level, &name);
                let value = if level == 0
                    && module_name.as_deref() == Some("importlib")
                    && name == "import_module"
                {
                    ValueSet::importlib_import_module()
                } else {
                    target.clone()
                };
                let changed = self.record_binding_value(binding, value);
                self.add_cross_references_from_value(
                    target.clone(),
                    FindingReferenceKind::Import,
                    module,
                    name.clone(),
                    range,
                    phase,
                );
                changed | self.record_package_reexport(module, binding, &name, target)
            }
            ImportOperationKind::StarImport { .. } => false,
        }
    }

    fn record_package_reexport(
        &mut self,
        module: ModuleId,
        binding: BindingId,
        imported_name: &str,
        target: ValueSet,
    ) -> bool {
        let Some(parsed) = self.facts.module_source(module) else {
            return false;
        };
        if !is_package_module(&parsed.module) {
            return false;
        }
        let Some(binding_fact) = self.facts.bindings.get(&binding) else {
            return false;
        };
        if self
            .facts
            .module_scopes
            .get(&binding_fact.module)
            .is_none_or(|scope| *scope != binding_fact.scope)
        {
            return false;
        }
        let kind = if binding_fact.name == imported_name {
            FindingExportKind::DirectReExport
        } else {
            FindingExportKind::AliasedReExport
        };
        self.record_exports_from_value(target, binding_fact.name.clone(), kind, module)
    }

    fn apply_dynamic_import_operation(&mut self, operation: DynamicImportOperation) -> bool {
        if !self.is_dynamic_import_function(operation.module, &operation.call.func) {
            return false;
        }
        let Some(value) = self.resolve_dynamic_import_call_value(operation.module, &operation.call)
        else {
            return self
                .module_uncertainty
                .entry(operation.module)
                .or_default()
                .insert(Part2Uncertainty::DynamicImport);
        };
        let mut changed = false;
        for module in value.modules {
            if let Some(name) = self.facts.module_names.get(&module).cloned() {
                changed |= self.add_parent_package_attributes(&name, module);
            }
        }
        changed
    }

    fn record_binding_value(&mut self, binding: BindingId, value: ValueSet) -> bool {
        let mut changed = self
            .binding_values
            .entry(binding)
            .or_default()
            .union_with(value.clone());
        if let Some(binding_fact) = self.facts.bindings.get(&binding) {
            if self
                .facts
                .module_scopes
                .get(&binding_fact.module)
                .is_some_and(|scope| *scope == binding_fact.scope)
            {
                changed |= self
                    .module_slots
                    .entry((binding_fact.module, binding_fact.name.clone()))
                    .or_default()
                    .union_with(value);
            }
        }
        changed
    }

    fn apply_attribute_operation(&mut self, operation: AttributeOperation) -> bool {
        let attr = operation.expression.attr.id.to_string();
        let base = self.resolve_expr_value(operation.module, &operation.expression.value);
        let mut changed = false;
        match operation.kind {
            AttributeOperationKind::Read => {
                let value = self.resolve_module_attribute(base, &attr);
                self.add_cross_references_from_value(
                    value,
                    FindingReferenceKind::ModuleAttribute,
                    operation.module,
                    attr,
                    to_range(operation.expression.range),
                    operation.phase,
                );
            }
            AttributeOperationKind::Write => {
                let had_modules = !base.modules.is_empty();
                for module in &base.modules {
                    changed |= self
                        .module_slots
                        .entry((*module, attr.clone()))
                        .or_default()
                        .uncertainties
                        .insert(Part2Uncertainty::NamespaceOrder);
                }
                if !had_modules && !base.is_empty() {
                    self.module_uncertainty
                        .entry(operation.module)
                        .or_default()
                        .insert(Part2Uncertainty::DynamicModuleAttribute);
                }
            }
            AttributeOperationKind::Delete => {
                for module in &base.modules {
                    changed |= self
                        .module_slots
                        .entry((*module, attr.clone()))
                        .or_default()
                        .uncertainties
                        .insert(Part2Uncertainty::NamespaceOrder);
                }
            }
        }
        changed
    }

    fn apply_star_import(&mut self, operation: ImportOperation) -> bool {
        let ImportOperation {
            module,
            range,
            kind,
            phase,
        } = operation;
        let ImportOperationKind::StarImport { module_name, level } = kind else {
            return false;
        };
        let Some(imported_module_name) =
            self.namespace
                .relative_module_name(module, level, module_name.as_deref())
        else {
            self.module_uncertainty
                .entry(module)
                .or_default()
                .insert(Part2Uncertainty::ImportResolution);
            return false;
        };
        let LocalModuleResolution::Module(imported_module) =
            self.namespace.resolve_absolute(&imported_module_name)
        else {
            self.module_uncertainty
                .entry(module)
                .or_default()
                .insert(Part2Uncertainty::ImportResolution);
            return false;
        };
        let surface = self.export_surface(imported_module);
        let mut changed = false;
        for (name, value) in surface {
            changed |= self
                .module_slots
                .entry((module, name.clone()))
                .or_default()
                .union_with(value.clone());
            self.add_cross_references_from_value(
                value,
                FindingReferenceKind::Import,
                module,
                name,
                range,
                phase,
            );
        }
        changed
    }

    fn apply_exports(&mut self) -> bool {
        let mut changed = false;
        for parsed in self.parsed_modules {
            let module = parsed.module.id;
            let export_state = module_all_state(&parsed.syntax);
            if export_state.uncertain {
                self.module_uncertainty
                    .entry(module)
                    .or_default()
                    .insert(Part2Uncertainty::DynamicExport);
            }
            for name in export_state.explicit_names {
                let value = self.module_slot_value(module, &name);
                changed |= self.record_exports_from_value(
                    value,
                    name,
                    FindingExportKind::ExplicitAll,
                    module,
                );
            }
            if is_package_module(&parsed.module) && self.mode != ProjectMode::Application {
                for (name, value) in self.public_module_slots(module) {
                    changed |= self.record_exports_from_value(
                        value,
                        name,
                        FindingExportKind::PackagePublicBinding,
                        module,
                    );
                }
            }
        }
        changed
    }

    fn record_exports_from_value(
        &mut self,
        value: ValueSet,
        public_name: String,
        kind: FindingExportKind,
        source_module: ModuleId,
    ) -> bool {
        let before = self.export_references.len();
        for definition in value.definitions {
            if !self.export_references.iter().any(|reference| {
                reference.definition == definition
                    && reference.public_name == public_name
                    && reference.kind == kind
                    && reference.source_module == source_module
            }) {
                self.export_references.push(ExportReference {
                    definition,
                    public_name: public_name.clone(),
                    kind,
                    source_module,
                });
                self.cross_references.push(CrossReference { definition });
            }
        }
        self.export_references.len() != before
    }

    fn resolve_import_from(
        &mut self,
        source_module: ModuleId,
        module_name: Option<&str>,
        level: u32,
        name: &str,
    ) -> ValueSet {
        let Some(base_name) =
            self.namespace
                .relative_module_name(source_module, level, module_name)
        else {
            return ValueSet::default().with_uncertainty(Part2Uncertainty::ImportResolution);
        };
        match self.namespace.resolve_absolute(&base_name) {
            LocalModuleResolution::Module(module) => {
                let value = self.module_slot_value(module, name);
                if !value.is_empty() {
                    return value;
                }
                let submodule = format!("{base_name}.{name}");
                match self.namespace.resolve_absolute(&submodule) {
                    LocalModuleResolution::Module(module) => {
                        self.add_parent_package_attributes(&submodule, module);
                        ValueSet::module(module)
                    }
                    LocalModuleResolution::Namespace(_) => {
                        ValueSet::default().with_uncertainty(Part2Uncertainty::UnsupportedNamespace)
                    }
                    LocalModuleResolution::External => {
                        ValueSet::external(Part2Uncertainty::ExternalImport)
                    }
                    LocalModuleResolution::Unsupported(_) => {
                        ValueSet::default().with_uncertainty(Part2Uncertainty::ImportResolution)
                    }
                }
            }
            LocalModuleResolution::Namespace(namespace) => {
                let submodule = format!("{namespace}.{name}");
                match self.namespace.resolve_absolute(&submodule) {
                    LocalModuleResolution::Module(module) => ValueSet::module(module),
                    _ => {
                        ValueSet::default().with_uncertainty(Part2Uncertainty::UnsupportedNamespace)
                    }
                }
            }
            LocalModuleResolution::External => ValueSet::external(Part2Uncertainty::ExternalImport),
            LocalModuleResolution::Unsupported(_) => {
                ValueSet::default().with_uncertainty(Part2Uncertainty::ImportResolution)
            }
        }
    }

    fn add_parent_package_attributes(&mut self, full_name: &str, module: ModuleId) -> bool {
        let Some((parent_name, attr)) = full_name.rsplit_once('.') else {
            return false;
        };
        if let LocalModuleResolution::Module(parent) = self.namespace.resolve_absolute(parent_name)
        {
            return self
                .module_slots
                .entry((parent, attr.to_owned()))
                .or_default()
                .union_with(
                    ValueSet::module(module)
                        .with_uncertainty(Part2Uncertainty::PartialInitialization)
                        .with_uncertainty(Part2Uncertainty::NamespaceOrder),
                );
        }
        false
    }

    fn module_slot_value(&self, module: ModuleId, name: &str) -> ValueSet {
        self.module_slots
            .get(&(module, name.to_owned()))
            .cloned()
            .unwrap_or_default()
    }

    fn public_module_slots(&self, module: ModuleId) -> Vec<(String, ValueSet)> {
        self.module_slots
            .iter()
            .filter(|((slot_module, name), _)| *slot_module == module && is_public_name(name))
            .map(|((_, name), value)| (name.clone(), value.clone()))
            .collect()
    }

    fn export_surface(&self, module: ModuleId) -> Vec<(String, ValueSet)> {
        if let Some(parsed) = self.facts.module_source(module) {
            let state = module_all_state(&parsed.syntax);
            if !state.explicit_names.is_empty() {
                let mut surface = if state.implicit_possible {
                    self.public_module_slots(module)
                        .into_iter()
                        .collect::<BTreeMap<_, _>>()
                } else {
                    BTreeMap::new()
                };
                for name in state.explicit_names {
                    surface.insert(name.clone(), self.module_slot_value(module, &name));
                }
                return surface.into_iter().collect();
            }
        }
        self.public_module_slots(module)
    }

    fn resolve_expr_value(&mut self, module: ModuleId, expression: &Expr) -> ValueSet {
        match expression {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                let Some(reference) = self.facts.reference_for_name(module, name) else {
                    return ValueSet::default();
                };
                self.resolve_reference_value(reference)
            }
            Expr::Attribute(attribute) => {
                let base = self.resolve_expr_value(module, &attribute.value);
                self.resolve_module_attribute(base, attribute.attr.id.as_str())
            }
            Expr::Call(call) => self
                .resolve_dynamic_import_call_value(module, call)
                .unwrap_or_default(),
            _ => ValueSet::default(),
        }
    }

    fn resolve_reference_value(&self, reference: &ReferenceFact) -> ValueSet {
        let mut value = ValueSet::default();
        for binding in reaching_bindings(self.graph, reference) {
            if let Some(provenance) = self.binding_values.get(&binding) {
                value.union_with(provenance.clone());
            }
            if let Some(definition) = self.facts.definitions_by_binding.get(&binding) {
                value.definitions.insert(*definition);
            }
        }
        value
    }

    fn resolve_module_attribute(&self, base: ValueSet, attr: &str) -> ValueSet {
        let mut value = ValueSet::default();
        if base.importlib_module && attr == "import_module" {
            value.union_with(ValueSet::importlib_import_module());
        }
        for module in base.modules {
            value.union_with(self.module_slot_value(module, attr));
            if self
                .module_uncertainty
                .get(&module)
                .is_some_and(|uncertainties| {
                    uncertainties.contains(&Part2Uncertainty::ModuleGetattr)
                })
            {
                value.uncertainties.insert(Part2Uncertainty::ModuleGetattr);
            }
        }
        if base.external {
            value.external = true;
            value.uncertainties.insert(Part2Uncertainty::ExternalImport);
        }
        value.uncertainties.extend(base.uncertainties);
        value
    }

    fn resolve_dynamic_import_call_value(
        &mut self,
        module: ModuleId,
        call: &ExprCall,
    ) -> Option<ValueSet> {
        if !self.is_dynamic_import_function(module, &call.func) {
            return None;
        }
        let module_name = first_string_arg(call)?;
        let returned_name = if is_default_dunder_import(call) {
            module_name
                .split('.')
                .next()
                .map(str::to_owned)
                .unwrap_or(module_name)
        } else {
            module_name
        };
        match self.namespace.resolve_absolute(&returned_name) {
            LocalModuleResolution::Module(target) => {
                self.add_parent_package_attributes(&returned_name, target);
                Some(ValueSet::module(target))
            }
            LocalModuleResolution::Namespace(_) => {
                Some(ValueSet::default().with_uncertainty(Part2Uncertainty::UnsupportedNamespace))
            }
            LocalModuleResolution::External => {
                Some(ValueSet::external(Part2Uncertainty::ExternalImport))
            }
            LocalModuleResolution::Unsupported(_) => {
                Some(ValueSet::default().with_uncertainty(Part2Uncertainty::ImportResolution))
            }
        }
    }

    fn is_dynamic_import_function(&mut self, module: ModuleId, expression: &Expr) -> bool {
        if let Expr::Name(name) = expression {
            if name.id.as_str() == "__import__"
                && self
                    .facts
                    .reference_for_name(module, name)
                    .is_none_or(|reference| reaching_bindings(self.graph, reference).is_empty())
            {
                return true;
            }
        }
        self.resolve_expr_value(module, expression)
            .importlib_import_module
    }

    fn add_cross_references_from_value(
        &mut self,
        value: ValueSet,
        _kind: FindingReferenceKind,
        source_module: ModuleId,
        _source: String,
        _range: TextRange,
        _phase: ReferencePhase,
    ) {
        for definition in value.definitions {
            if !self
                .cross_references
                .iter()
                .any(|reference| reference.definition == definition)
            {
                self.cross_references.push(CrossReference { definition });
            }
        }
        if !value.uncertainties.is_empty() {
            self.module_uncertainty
                .entry(source_module)
                .or_default()
                .extend(value.uncertainties);
        }
    }

    fn is_selected_module(&self, module: ModuleId) -> bool {
        let name = self.facts.module_name(module);
        self.namespace.module_for_name(&name) == Some(module)
    }
}

#[derive(Clone, Debug)]
struct AllState {
    explicit_names: BTreeSet<String>,
    implicit_possible: bool,
    uncertain: bool,
}

fn module_all_state(module: &ModModule) -> AllState {
    let mut state = AllState {
        explicit_names: BTreeSet::new(),
        implicit_possible: true,
        uncertain: false,
    };
    apply_all_statements(&mut state, &module.body);
    state
}

fn apply_all_statements(state: &mut AllState, statements: &[Stmt]) {
    for statement in statements {
        match statement {
            Stmt::Assign(assign) if assigns_name(&assign.targets, "__all__") => {
                if let Some(names) = literal_string_sequence(&assign.value) {
                    state.explicit_names = names.into_iter().collect();
                    state.implicit_possible = false;
                } else {
                    state.uncertain = true;
                }
            }
            Stmt::AugAssign(assign) if target_name(&assign.target) == Some("__all__") => {
                state.uncertain = true;
            }
            Stmt::Expr(expr) if mutates_all(&expr.value) => {
                state.uncertain = true;
            }
            Stmt::If(if_stmt) => {
                let mut body = state.clone();
                apply_all_statements(&mut body, &if_stmt.body);
                let mut alternatives = Vec::new();
                alternatives.push(body);
                let mut has_else = false;
                for clause in &if_stmt.elif_else_clauses {
                    has_else |= clause.test.is_none();
                    let mut branch = state.clone();
                    apply_all_statements(&mut branch, &clause.body);
                    alternatives.push(branch);
                }
                if !has_else {
                    alternatives.push(state.clone());
                }
                let mut merged = AllState {
                    explicit_names: BTreeSet::new(),
                    implicit_possible: false,
                    uncertain: false,
                };
                for branch in alternatives {
                    merged.explicit_names.extend(branch.explicit_names);
                    merged.implicit_possible |= branch.implicit_possible;
                    merged.uncertain |= branch.uncertain;
                }
                *state = merged;
            }
            _ => {}
        }
    }
}

fn build_findings(
    graph: &SemanticGraph,
    parsed_modules: &[ParsedProjectModule],
    facts: &ProjectFacts<'_>,
    resolver: &ProjectResolver<'_, '_>,
    mode: ProjectMode,
) -> Vec<Finding> {
    let mut same_module_refs = same_module_references(graph, facts);
    let mut cross_refs: BTreeMap<DefId, Vec<&CrossReference>> = BTreeMap::new();
    for reference in &resolver.cross_references {
        cross_refs
            .entry(reference.definition)
            .or_default()
            .push(reference);
    }
    let mut exports_by_definition: BTreeMap<DefId, Vec<&ExportReference>> = BTreeMap::new();
    for export in &resolver.export_references {
        exports_by_definition
            .entry(export.definition)
            .or_default()
            .push(export);
    }
    let mut source_by_module = BTreeMap::new();
    for parsed in parsed_modules {
        source_by_module.insert(parsed.module.id, parsed);
    }

    let mut findings = Vec::new();
    for definition in graph
        .definitions
        .iter()
        .filter(|definition| definition.reportable)
    {
        if !resolver.is_selected_module(definition.module) {
            continue;
        }
        if definition.role == cull_core::DefinitionRole::OverloadDeclaration {
            continue;
        }
        let surface = definition_surface(
            definition,
            exports_by_definition.contains_key(&definition.id),
        );
        if surface == DefinitionSurface::ModuleProtocolHook {
            continue;
        }
        if exports_by_definition.contains_key(&definition.id) {
            continue;
        }
        if same_module_refs
            .remove(&definition.id)
            .is_some_and(|references| !references.is_empty())
        {
            continue;
        }
        if cross_refs
            .get(&definition.id)
            .is_some_and(|references| !references.is_empty())
        {
            continue;
        }

        let (mut confidence, mut reason) = confidence_for_surface(mode, surface);
        let mut uncertainty = resolver
            .module_uncertainty
            .get(&definition.module)
            .into_iter()
            .flatten()
            .map(|uncertainty| FindingUncertainty {
                kind: uncertainty.output_kind(),
                detail: uncertainty.detail().to_owned(),
            })
            .collect::<Vec<_>>();
        if confidence == FindingConfidence::Review {
            uncertainty.push(FindingUncertainty {
                kind: FindingUncertaintyKind::PublicSurfacePolicy,
                detail: reason.clone(),
            });
        }
        if confidence == FindingConfidence::High && !uncertainty.is_empty() {
            confidence = FindingConfidence::Review;
            reason = "analysis uncertainty prevents a high-confidence claim".to_owned();
        }

        let Some(parsed) = source_by_module.get(&definition.module) else {
            continue;
        };
        let (line, column) = line_column(&parsed.source_text, definition.name_range.start);
        let rule_id = match definition.kind {
            DefinitionKind::Function => FindingRule::Cull001,
            DefinitionKind::Class => FindingRule::Cull002,
        };
        let effects = definition_effects(definition, facts);
        let finding = Finding {
            id: format!(
                "{}:{}:{}",
                rule_id.code(),
                parsed.module.display_path,
                definition.name
            ),
            rule_id,
            finding_type: FindingType::Unreferenced,
            definition: FindingDefinition {
                kind: definition.kind,
                name: definition.name.clone(),
                qualified_name: definition.qualified_name.clone(),
                module: facts.module_name(definition.module),
                file: parsed.module.display_path.clone(),
                range: definition.range,
                line,
                column,
            },
            confidence,
            inbound_references: Vec::new(),
            reachability: FindingReachability {
                status: FindingReachabilityStatus::NotComputed,
            },
            exports: Vec::new(),
            mode_effect: FindingModeEffect {
                mode,
                surface,
                confidence_ceiling: confidence,
                reason,
            },
            uncertainty,
            origin_domains: vec![FindingOriginSummary {
                origin_domain: definition.origin_domain,
                count: 1,
            }],
            reference_phases: Vec::new(),
            removal_risk: FindingRemovalRisk::from_semantic(&definition.removal_risk, &effects),
            explanation: explanation_for(definition, confidence, surface),
        };
        findings.push(finding);
    }

    findings.sort_by(|left, right| {
        left.definition
            .file
            .cmp(&right.definition.file)
            .then(
                left.definition
                    .range
                    .start
                    .cmp(&right.definition.range.start),
            )
            .then(left.rule_id.cmp(&right.rule_id))
            .then(left.id.cmp(&right.id))
    });
    findings
}

fn same_module_references<'a>(
    graph: &'a SemanticGraph,
    facts: &ProjectFacts<'a>,
) -> BTreeMap<DefId, Vec<&'a ReferenceFact>> {
    let mut refs: BTreeMap<DefId, Vec<&ReferenceFact>> = BTreeMap::new();
    for reference in &graph.references {
        for binding in reaching_bindings(graph, reference) {
            if let Some(definition) = facts.definitions_by_binding.get(&binding) {
                refs.entry(*definition).or_default().push(reference);
            }
        }
    }
    refs
}

fn reaching_bindings(graph: &SemanticGraph, reference: &ReferenceFact) -> Vec<BindingId> {
    let cull_core::ReferenceBindingState::Analyzed(state) = &reference.binding_state else {
        return Vec::new();
    };
    if state.reachability == LocalReachability::Unreachable {
        return Vec::new();
    }
    graph
        .binding_sets
        .iter()
        .find(|set| set.id == state.bindings)
        .map(|set| set.bindings.clone())
        .unwrap_or_default()
}

fn definition_surface(definition: &SemanticDefinition, exported: bool) -> DefinitionSurface {
    if exported {
        return DefinitionSurface::Exported;
    }
    if matches!(definition.name.as_str(), "__getattr__" | "__dir__") {
        return DefinitionSurface::ModuleProtocolHook;
    }
    if is_dunder(&definition.name) {
        return DefinitionSurface::SpecialDunder;
    }
    if definition.name.starts_with('_') {
        DefinitionSurface::Private
    } else {
        DefinitionSurface::Public
    }
}

fn confidence_for_surface(
    mode: ProjectMode,
    surface: DefinitionSurface,
) -> (FindingConfidence, String) {
    match surface {
        DefinitionSurface::Private => (
            FindingConfidence::High,
            "private definition has no inbound references under Cull's static model".to_owned(),
        ),
        DefinitionSurface::Public if mode == ProjectMode::Application => (
            FindingConfidence::High,
            "application mode allows public definitions to be high confidence".to_owned(),
        ),
        DefinitionSurface::Public => (
            FindingConfidence::Review,
            "public definition is conservative outside explicit application mode".to_owned(),
        ),
        DefinitionSurface::SpecialDunder => (
            FindingConfidence::Review,
            "special dunder definitions are at most Review in v0".to_owned(),
        ),
        DefinitionSurface::Exported | DefinitionSurface::ModuleProtocolHook => (
            FindingConfidence::Review,
            "surface is not normally reportable".to_owned(),
        ),
    }
}

fn explanation_for(
    definition: &SemanticDefinition,
    confidence: FindingConfidence,
    surface: DefinitionSurface,
) -> Vec<String> {
    let mut explanation = vec![
        "no resolved inbound references were found".to_owned(),
        "root reachability was not computed in Part 2".to_owned(),
        format!("definition surface: {surface:?}"),
    ];
    if confidence == FindingConfidence::Review {
        explanation.push("mode or uncertainty prevents a high-confidence claim".to_owned());
    }
    if matches!(
        definition.removal_risk,
        RemovalRisk::Review(_) | RemovalRisk::Unknown
    ) {
        explanation.push("removal risk is reported separately from unusedness".to_owned());
    }
    explanation
}

fn definition_effects(
    definition: &SemanticDefinition,
    facts: &ProjectFacts<'_>,
) -> Vec<DefinitionEffectKind> {
    facts
        .effect_sets
        .get(&definition.definition_effects)
        .cloned()
        .unwrap_or_default()
}

fn summarize_findings(findings: &[Finding]) -> CheckSummary {
    CheckSummary {
        high_confidence: findings
            .iter()
            .filter(|finding| finding.confidence == FindingConfidence::High)
            .count(),
        review: findings
            .iter()
            .filter(|finding| finding.confidence == FindingConfidence::Review)
            .count(),
        suppressed: 0,
    }
}

fn compare_diagnostics(left: &Diagnostic, right: &Diagnostic) -> std::cmp::Ordering {
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
}

fn line_column(source: &str, byte_start: u32) -> (u32, u32) {
    let mut line = 1u32;
    let mut column = 1u32;
    for (index, byte) in source.bytes().enumerate() {
        if index as u32 >= byte_start {
            break;
        }
        if byte == b'\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn is_public_name(name: &str) -> bool {
    !name.starts_with('_')
}

fn is_dunder(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__") && name.len() > 4
}

fn is_type_checking_expr(expression: &Expr) -> bool {
    match expression {
        Expr::Name(name) => name.id.as_str() == "TYPE_CHECKING",
        Expr::Attribute(attribute) => {
            attribute.attr.id.as_str() == "TYPE_CHECKING"
                && matches!(&*attribute.value, Expr::Name(name) if matches!(name.id.as_str(), "typing" | "typing_extensions"))
        }
        _ => false,
    }
}

fn collect_arguments<'a>(arguments: &'a Arguments, mut visit: impl FnMut(&'a Expr)) {
    for arg in &arguments.args {
        visit(arg);
    }
    for keyword in &arguments.keywords {
        visit(&keyword.value);
    }
}

fn first_string_arg(call: &ExprCall) -> Option<String> {
    let first = call.arguments.args.first()?;
    string_literal_value(first)
}

fn string_literal_value(expression: &Expr) -> Option<String> {
    match expression {
        Expr::StringLiteral(literal) => Some(literal.value.to_str().to_owned()),
        _ => None,
    }
}

fn is_default_dunder_import(call: &ExprCall) -> bool {
    if !matches!(&*call.func, Expr::Name(name) if name.id.as_str() == "__import__") {
        return false;
    }
    !call.arguments.keywords.iter().any(|keyword| {
        keyword
            .arg
            .as_ref()
            .is_some_and(|arg| arg.id.as_str() == "fromlist")
    }) && call.arguments.args.len() < 4
}

fn assigns_name(targets: &[Expr], expected: &str) -> bool {
    targets
        .iter()
        .any(|target| target_name(target) == Some(expected))
}

fn target_name(target: &Expr) -> Option<&str> {
    match target {
        Expr::Name(name) => Some(name.id.as_str()),
        _ => None,
    }
}

fn literal_string_sequence(expression: &Expr) -> Option<Vec<String>> {
    let elements = match expression {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    let mut names = Vec::new();
    for element in elements {
        names.push(string_literal_value(element)?);
    }
    Some(names)
}

fn mutates_all(expression: &Expr) -> bool {
    let Expr::Call(call) = expression else {
        return false;
    };
    matches!(
        &*call.func,
        Expr::Attribute(attribute)
            if matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "__all__")
    )
}
