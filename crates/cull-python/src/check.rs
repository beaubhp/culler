use std::collections::{BTreeMap, BTreeSet};

use crate::{
    analysis::{ParsedProjectModule, SemanticProjectData},
    discovery::DiscoveredProject,
    module_namespace::{is_package_module, LocalModuleResolution, ModuleNamespaceIndex},
    ruff_frontend::to_range,
};
use cull_core::{
    BindingFact, BindingId, BindingKind, Candidate, CandidateStatus, CheckAnalysis, CheckOutput,
    CheckSummary, DefId, DefinitionEffectKind, DefinitionKind, DefinitionSurface, Diagnostic,
    DiagnosticSeverity, EvidenceKind, EvidenceRecord, Finding, FindingBlocker, FindingBlockerKind,
    FindingConfidence, FindingDefinition, FindingExportKind, FindingModeEffect,
    FindingOriginSummary, FindingReachability, FindingReachabilityStatus, FindingReferenceKind,
    FindingRemovalRisk, FindingRule, FindingType, FindingUncertainty, FindingUncertaintyKind,
    LocalReachability, ModuleId, OriginDomain, ProjectCompleteness, ProjectMode,
    ReachabilityDomain, ReferenceFact, ReferencePhase, RemovalRisk, RootCoverage, RootId,
    RootInvocation, RootKind, RootOutput, SecondaryCondition, SemanticDefinition, SemanticGraph,
    SuppressionReason, SuppressionReasonKind, TextRange, UncertaintyEffect, UncertaintyRegion,
};
use data_encoding::BASE32_NOPAD;
use ruff_python_ast::{
    Arguments, CmpOp, Expr, ExprAttribute, ExprCall, ExprContext, ExprName, ModModule, Stmt,
    StmtAssign, StmtAugAssign, StmtDelete, StmtImport, StmtImportFrom,
};

const CHECK_SCHEMA_VERSION: u32 = 2;

fn check_analysis(
    mode: ProjectMode,
    target_python: cull_core::PythonVersion,
    root_coverage: RootCoverage,
) -> CheckAnalysis {
    CheckAnalysis {
        mode,
        target_python,
        root_coverage,
    }
}

fn apply_partial_analysis_policy(
    diagnostics: &mut Vec<Diagnostic>,
    project_completeness: &ProjectCompleteness,
    allow_partial: bool,
) {
    if !allow_partial
        || project_completeness.status != cull_core::ProjectCompletenessStatus::Partial
    {
        return;
    }

    let skipped = project_completeness
        .skipped_files
        .iter()
        .map(|file| (file.path.as_str(), file.diagnostic_code.as_str()))
        .collect::<BTreeSet<_>>();
    for diagnostic in diagnostics.iter_mut() {
        let Some(path) = diagnostic.path.as_deref() else {
            continue;
        };
        if skipped.contains(&(path, diagnostic.code.as_str()))
            || is_skippable_file_error(diagnostic)
        {
            diagnostic.severity = DiagnosticSeverity::Warning;
        }
    }
    diagnostics.push(Diagnostic::warning(
        "CULL_P0400",
        format!(
            "partial project analysis skipped {} included source file(s); high-confidence findings are capped at Review",
            project_completeness.skipped_files.len()
        ),
    ));
}

fn is_skippable_file_error(diagnostic: &Diagnostic) -> bool {
    matches!(
        diagnostic.code.as_str(),
        "CULL_P0100" | "CULL_P0101" | "CULL_P0200" | "CULL_P0201" | "CULL_P0202"
    )
}

pub(crate) fn analyze_project(
    data: SemanticProjectData,
    mode_override: Option<ProjectMode>,
    allow_partial_override: bool,
) -> CheckOutput {
    let mode = mode_override.unwrap_or(data.project.mode);
    let allow_partial = allow_partial_override || data.project.allow_partial;
    let mut diagnostics = data.diagnostics;
    let mut namespace = ModuleNamespaceIndex::build(&data.project);
    let project_completeness = data.project_completeness;
    apply_partial_analysis_policy(&mut diagnostics, &project_completeness, allow_partial);

    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        return CheckOutput {
            schema_version: CHECK_SCHEMA_VERSION,
            analysis: check_analysis(mode, data.project.target_python, RootCoverage::Absent),
            project_completeness,
            target_python: data.project.target_python,
            project_root: crate::paths::slash_path(&data.project.project_root),
            source_roots: data.project.source_root_output(),
            mode,
            root_coverage: RootCoverage::Absent,
            roots: Vec::new(),
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
    let reachability = ReachabilityBuilder::analyze(
        &data.graph,
        &data.project,
        &data.parsed_modules,
        &facts,
        &mut resolver,
        mode,
        &mut diagnostics,
    );
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        diagnostics.sort_by(compare_diagnostics);
        return CheckOutput {
            schema_version: CHECK_SCHEMA_VERSION,
            analysis: check_analysis(mode, data.project.target_python, reachability.root_coverage),
            project_completeness,
            target_python: data.project.target_python,
            project_root: crate::paths::slash_path(&data.project.project_root),
            source_roots: data.project.source_root_output(),
            mode,
            root_coverage: reachability.root_coverage,
            roots: reachability.roots,
            findings: Vec::new(),
            summary: CheckSummary::default(),
            diagnostics,
        };
    }
    let findings = build_findings(
        &data.graph,
        &data.parsed_modules,
        &facts,
        &resolver,
        &reachability,
        mode,
        &project_completeness,
    );
    let summary = summarize_findings(&findings);
    diagnostics.sort_by(compare_diagnostics);

    CheckOutput {
        schema_version: CHECK_SCHEMA_VERSION,
        analysis: check_analysis(mode, data.project.target_python, reachability.root_coverage),
        project_completeness,
        target_python: data.project.target_python,
        project_root: crate::paths::slash_path(&data.project.project_root),
        source_roots: data.project.source_root_output(),
        mode,
        root_coverage: reachability.root_coverage,
        roots: reachability.roots,
        findings,
        summary,
        diagnostics,
    }
}

struct ProjectFacts<'a> {
    modules: BTreeMap<ModuleId, &'a ParsedProjectModule>,
    module_names: BTreeMap<ModuleId, String>,
    module_scopes: BTreeMap<ModuleId, cull_core::ScopeId>,
    module_contexts: BTreeMap<ModuleId, cull_core::ContextId>,
    definitions_by_id: BTreeMap<DefId, &'a SemanticDefinition>,
    definitions_by_binding: BTreeMap<BindingId, DefId>,
    bindings: BTreeMap<BindingId, &'a BindingFact>,
    bindings_by_symbol: BTreeMap<cull_core::SymbolId, Vec<BindingId>>,
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
        let module_contexts = graph
            .modules
            .iter()
            .map(|module| (module.id, module.context))
            .collect::<BTreeMap<_, _>>();

        let mut definitions_by_id = BTreeMap::new();
        let mut definitions_by_binding = BTreeMap::new();
        for definition in &graph.definitions {
            definitions_by_id.insert(definition.id, definition);
            definitions_by_binding.insert(definition.binding, definition.id);
        }

        let mut bindings = BTreeMap::new();
        let mut bindings_by_symbol = BTreeMap::new();
        let mut bindings_by_module_name_range_kind = BTreeMap::new();
        for binding in &graph.bindings {
            bindings.insert(binding.id, binding);
            bindings_by_symbol
                .entry(binding.symbol)
                .or_insert_with(Vec::new)
                .push(binding.id);
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
            module_contexts,
            definitions_by_id,
            definitions_by_binding,
            bindings,
            bindings_by_symbol,
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

    fn module_context(&self, module: ModuleId) -> Option<cull_core::ContextId> {
        self.module_contexts.get(&module).copied()
    }

    fn definition(&self, definition: DefId) -> Option<&'a SemanticDefinition> {
        self.definitions_by_id.get(&definition).copied()
    }

    fn definition_for_name_range(
        &self,
        module: ModuleId,
        range: TextRange,
        kind: BindingKind,
    ) -> Option<DefId> {
        self.binding_for_alias(module, range, kind)
            .and_then(|binding| self.definitions_by_binding.get(&binding).copied())
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
    DynamicAttributeRead,
    DynamicExecution,
    DynamicExport,
    DynamicImport,
    DynamicModuleAttribute,
    ExternalImport,
    ImportResolution,
    ModuleGetattr,
    NamespaceOrder,
    NamespaceMutation,
    PartialInitialization,
    RuntimeAnnotationIntrospection,
    UnsupportedNamespace,
}

impl Part2Uncertainty {
    fn output_kind(self) -> FindingUncertaintyKind {
        match self {
            Self::DynamicAttributeRead => FindingUncertaintyKind::DynamicAttributeRead,
            Self::DynamicExecution => FindingUncertaintyKind::DynamicExecution,
            Self::DynamicExport => FindingUncertaintyKind::DynamicExport,
            Self::DynamicImport => FindingUncertaintyKind::DynamicImport,
            Self::DynamicModuleAttribute => FindingUncertaintyKind::DynamicModuleAttribute,
            Self::ExternalImport => FindingUncertaintyKind::ExternalImport,
            Self::ImportResolution => FindingUncertaintyKind::ImportResolution,
            Self::ModuleGetattr => FindingUncertaintyKind::ModuleGetattr,
            Self::NamespaceOrder => FindingUncertaintyKind::NamespaceOrder,
            Self::NamespaceMutation => FindingUncertaintyKind::NamespaceMutation,
            Self::PartialInitialization => FindingUncertaintyKind::PartialInitialization,
            Self::RuntimeAnnotationIntrospection => {
                FindingUncertaintyKind::RuntimeAnnotationIntrospection
            }
            Self::UnsupportedNamespace => FindingUncertaintyKind::UnsupportedNamespace,
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::DynamicAttributeRead => {
                "dynamic attribute read may reference unknown names in a known namespace"
            }
            Self::DynamicExecution => {
                "dynamic code execution may introduce references in this module"
            }
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
            Self::NamespaceMutation => {
                "dynamic namespace mutation may add, replace, or delete attributes"
            }
            Self::PartialInitialization => {
                "circular import may observe a partially initialized module"
            }
            Self::RuntimeAnnotationIntrospection => {
                "runtime annotation introspection may evaluate deferred annotation references"
            }
            Self::UnsupportedNamespace => "namespace provider could not be fully modeled",
        }
    }

    fn effects(self) -> Vec<UncertaintyEffect> {
        match self {
            Self::DynamicAttributeRead => vec![
                UncertaintyEffect::MayReadAnyAttribute,
                UncertaintyEffect::MayIntroduceReference,
            ],
            Self::DynamicExecution => vec![
                UncertaintyEffect::MayIntroduceReference,
                UncertaintyEffect::MayMutateNamespace,
                UncertaintyEffect::MayInvokeCallable,
            ],
            Self::DynamicExport => vec![
                UncertaintyEffect::MayAlterExports,
                UncertaintyEffect::MayIntroduceReference,
            ],
            Self::DynamicImport => vec![
                UncertaintyEffect::MayAlterRoots,
                UncertaintyEffect::MayIntroduceReference,
            ],
            Self::DynamicModuleAttribute | Self::ModuleGetattr => {
                vec![UncertaintyEffect::MayReadAnyAttribute]
            }
            Self::NamespaceMutation => vec![
                UncertaintyEffect::MayMutateNamespace,
                UncertaintyEffect::MayIntroduceReference,
            ],
            Self::ExternalImport | Self::ImportResolution | Self::UnsupportedNamespace => vec![
                UncertaintyEffect::MayIntroduceReference,
                UncertaintyEffect::MayAlterRoots,
            ],
            Self::NamespaceOrder | Self::PartialInitialization => {
                vec![UncertaintyEffect::MayIntroduceReference]
            }
            Self::RuntimeAnnotationIntrospection => {
                vec![UncertaintyEffect::MayEvaluateAnnotations]
            }
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

#[derive(Clone)]
struct ReflectiveOperation {
    module: ModuleId,
    call: ExprCall,
    kind: ReflectiveOperationKind,
    phase: ReferencePhase,
}

#[derive(Clone)]
struct NamespaceMappingOperation {
    module: ModuleId,
    call: ExprCall,
    kind: NamespaceMappingOperationKind,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReflectiveOperationKind {
    Getattr,
    Hasattr,
    Setattr,
    Delattr,
    Eval,
    Exec,
    RuntimeAnnotationIntrospection,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum NamespaceMappingOperationKind {
    Read,
    Mutation,
    Escape,
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
    reflective_operations: Vec<ReflectiveOperation>,
    namespace_mapping_operations: Vec<NamespaceMappingOperation>,
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
            reflective_operations: Vec::new(),
            namespace_mapping_operations: Vec::new(),
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
                    self.collect_namespace_mapping_escape(
                        module,
                        &assign.value,
                        NamespaceMappingOperationKind::Escape,
                    );
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
            Expr::Subscript(subscript) => {
                self.collect_namespace_mapping_escape(
                    module,
                    &subscript.value,
                    NamespaceMappingOperationKind::Mutation,
                );
                self.collect_expr_operations(module, &subscript.value, phase);
                self.collect_expr_operations(module, &subscript.slice, phase);
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
                    self.collect_namespace_mapping_escape(
                        module,
                        value,
                        NamespaceMappingOperationKind::Escape,
                    );
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
                self.collect_reflective_call(module, call, phase);
                self.collect_expr_operations(module, &call.func, phase);
                collect_arguments(&call.arguments, |expr| {
                    self.collect_namespace_mapping_escape(
                        module,
                        expr,
                        NamespaceMappingOperationKind::Escape,
                    );
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
                if matches!(expr.ctx, ExprContext::Load) {
                    self.collect_namespace_mapping_escape(
                        module,
                        &expr.value,
                        NamespaceMappingOperationKind::Read,
                    );
                }
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

    fn collect_reflective_call(
        &mut self,
        module: ModuleId,
        call: &ExprCall,
        phase: ReferencePhase,
    ) {
        let Some(kind) = self.reflective_call_kind(module, &call.func) else {
            return;
        };
        self.reflective_operations.push(ReflectiveOperation {
            module,
            call: call.clone(),
            kind,
            phase,
        });
    }

    fn collect_namespace_mapping_escape(
        &mut self,
        module: ModuleId,
        expression: &Expr,
        kind: NamespaceMappingOperationKind,
    ) {
        let Some(call) = namespace_mapping_call(expression) else {
            return;
        };
        self.namespace_mapping_operations
            .push(NamespaceMappingOperation {
                module,
                call: call.clone(),
                kind,
            });
    }

    fn reflective_call_kind(
        &self,
        _module: ModuleId,
        function: &Expr,
    ) -> Option<ReflectiveOperationKind> {
        match function {
            Expr::Name(name) => match name.id.as_str() {
                "getattr" => Some(ReflectiveOperationKind::Getattr),
                "hasattr" => Some(ReflectiveOperationKind::Hasattr),
                "setattr" => Some(ReflectiveOperationKind::Setattr),
                "delattr" => Some(ReflectiveOperationKind::Delattr),
                "eval" => Some(ReflectiveOperationKind::Eval),
                "exec" => Some(ReflectiveOperationKind::Exec),
                _ => None,
            },
            Expr::Attribute(attribute) => {
                let attr = attribute.attr.id.as_str();
                if attr == "get_annotations" || attr == "get_type_hints" || attr == "evaluate" {
                    return Some(ReflectiveOperationKind::RuntimeAnnotationIntrospection);
                }
                None
            }
            _ => None,
        }
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
            for operation in self.reflective_operations.clone() {
                changed |= self.apply_reflective_operation(operation);
            }
            for operation in self.namespace_mapping_operations.clone() {
                changed |= self.apply_namespace_mapping_operation(operation);
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

    fn apply_reflective_operation(&mut self, operation: ReflectiveOperation) -> bool {
        if self.reflective_builtin_is_shadowed(operation.module, &operation.call.func) {
            return false;
        }
        match operation.kind {
            ReflectiveOperationKind::Getattr | ReflectiveOperationKind::Hasattr => {
                self.apply_reflective_attribute_read(operation)
            }
            ReflectiveOperationKind::Setattr | ReflectiveOperationKind::Delattr => {
                self.apply_reflective_attribute_mutation(operation)
            }
            ReflectiveOperationKind::Eval | ReflectiveOperationKind::Exec => self
                .module_uncertainty
                .entry(operation.module)
                .or_default()
                .insert(Part2Uncertainty::DynamicExecution),
            ReflectiveOperationKind::RuntimeAnnotationIntrospection => self
                .module_uncertainty
                .entry(operation.module)
                .or_default()
                .insert(Part2Uncertainty::RuntimeAnnotationIntrospection),
        }
    }

    fn apply_reflective_attribute_read(&mut self, operation: ReflectiveOperation) -> bool {
        let Some(receiver) = operation.call.arguments.args.first() else {
            return false;
        };
        let base = self.resolve_expr_value(operation.module, receiver);
        let Some(name) = nth_string_arg(&operation.call, 1) else {
            return self.add_dynamic_namespace_uncertainty(
                operation.module,
                base,
                Part2Uncertainty::DynamicAttributeRead,
            );
        };
        let mut changed = false;
        if base.modules.is_empty() {
            if !base.is_empty() {
                changed |= self
                    .module_uncertainty
                    .entry(operation.module)
                    .or_default()
                    .insert(Part2Uncertainty::DynamicAttributeRead);
            }
            return changed;
        }
        for module in base.modules {
            let value = self.resolve_module_attribute(ValueSet::module(module), &name);
            self.add_cross_references_from_value(
                value,
                FindingReferenceKind::ModuleAttribute,
                operation.module,
                name.clone(),
                to_range(operation.call.range),
                operation.phase,
            );
        }
        changed
    }

    fn apply_reflective_attribute_mutation(&mut self, operation: ReflectiveOperation) -> bool {
        let Some(receiver) = operation.call.arguments.args.first() else {
            return false;
        };
        let base = self.resolve_expr_value(operation.module, receiver);
        let uncertainty = if nth_string_arg(&operation.call, 1).is_some() {
            Part2Uncertainty::NamespaceMutation
        } else {
            Part2Uncertainty::DynamicModuleAttribute
        };
        self.add_dynamic_namespace_uncertainty(operation.module, base, uncertainty)
    }

    fn apply_namespace_mapping_operation(&mut self, operation: NamespaceMappingOperation) -> bool {
        if self.reflective_builtin_is_shadowed(operation.module, &operation.call.func) {
            return false;
        }
        let uncertainties: &[Part2Uncertainty] = match operation.kind {
            NamespaceMappingOperationKind::Read => &[Part2Uncertainty::DynamicAttributeRead],
            NamespaceMappingOperationKind::Mutation => &[Part2Uncertainty::NamespaceMutation],
            NamespaceMappingOperationKind::Escape => &[
                Part2Uncertainty::DynamicAttributeRead,
                Part2Uncertainty::NamespaceMutation,
            ],
        };
        let entry = self.module_uncertainty.entry(operation.module).or_default();
        let mut changed = false;
        for uncertainty in uncertainties {
            changed |= entry.insert(*uncertainty);
        }
        changed
    }

    fn add_dynamic_namespace_uncertainty(
        &mut self,
        fallback_module: ModuleId,
        base: ValueSet,
        uncertainty: Part2Uncertainty,
    ) -> bool {
        let mut changed = false;
        if base.modules.is_empty() {
            changed |= self
                .module_uncertainty
                .entry(fallback_module)
                .or_default()
                .insert(uncertainty);
        } else {
            for module in base.modules {
                changed |= self
                    .module_uncertainty
                    .entry(module)
                    .or_default()
                    .insert(uncertainty);
            }
        }
        changed
    }

    fn reflective_builtin_is_shadowed(&mut self, module: ModuleId, function: &Expr) -> bool {
        matches!(function, Expr::Name(_)) && !self.resolve_expr_value(module, function).is_empty()
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

    fn resolve_expr_value_static(&self, module: ModuleId, expression: &Expr) -> ValueSet {
        match expression {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                let Some(reference) = self.facts.reference_for_name(module, name) else {
                    return ValueSet::default();
                };
                self.resolve_reference_value(reference)
            }
            Expr::Attribute(attribute) => {
                let base = self.resolve_expr_value_static(module, &attribute.value);
                self.resolve_module_attribute(base, attribute.attr.id.as_str())
            }
            _ => ValueSet::default(),
        }
    }

    fn resolve_reference_value(&self, reference: &ReferenceFact) -> ValueSet {
        let mut value = ValueSet::default();
        for binding in reaching_bindings(self.graph, self.facts, reference) {
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
                    .is_none_or(|reference| {
                        reaching_bindings(self.graph, self.facts, reference).is_empty()
                    })
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum ModuleExecutionMode {
    Imported,
    TopLevel,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum WorkItem {
    Module {
        domain: ReachabilityDomain,
        module: ModuleId,
        mode: ModuleExecutionMode,
    },
    Context {
        domain: ReachabilityDomain,
        context: cull_core::ContextId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExprUse {
    Runtime,
    Call,
    Escape,
}

#[derive(Clone, Debug, Default)]
struct ScanLocals {
    instance_classes: BTreeMap<String, BTreeSet<DefId>>,
}

#[derive(Clone, Debug)]
struct ReachabilityAnalysis {
    root_coverage: RootCoverage,
    roots: Vec<RootOutput>,
    production_values: BTreeSet<DefId>,
    production_contexts: BTreeSet<cull_core::ContextId>,
    test_values: BTreeSet<DefId>,
    test_contexts: BTreeSet<cull_core::ContextId>,
    external_values: BTreeSet<DefId>,
    external_contexts: BTreeSet<cull_core::ContextId>,
    graph_edges: BTreeMap<DefId, BTreeSet<DefId>>,
    dynamic_class_construction: BTreeSet<DefId>,
}

impl ReachabilityAnalysis {
    fn production_reachable(&self, definition: &SemanticDefinition) -> bool {
        match definition.kind {
            DefinitionKind::Function => self.production_contexts.contains(&definition.context),
            DefinitionKind::Class => self.production_values.contains(&definition.id),
        }
    }

    fn test_reachable(&self, definition: &SemanticDefinition) -> bool {
        match definition.kind {
            DefinitionKind::Function => self.test_contexts.contains(&definition.context),
            DefinitionKind::Class => self.test_values.contains(&definition.id),
        }
    }

    fn external_surface_reachable(&self, definition: &SemanticDefinition) -> bool {
        match definition.kind {
            DefinitionKind::Function => self.external_contexts.contains(&definition.context),
            DefinitionKind::Class => self.external_values.contains(&definition.id),
        }
    }

    fn roots_considered(&self) -> Vec<String> {
        self.roots
            .iter()
            .filter(|root| root.domain == ReachabilityDomain::Production && root.resolved)
            .map(|root| root.target.clone())
            .collect()
    }
}

struct ReachabilityBuilder<'a, 'b, 'c> {
    project: &'a DiscoveredProject,
    graph: &'a SemanticGraph,
    parsed_modules: &'a [ParsedProjectModule],
    facts: &'a ProjectFacts<'a>,
    resolver: &'b mut ProjectResolver<'a, 'c>,
    mode: ProjectMode,
    diagnostics: &'b mut Vec<Diagnostic>,
    roots: Vec<RootOutput>,
    production_values: BTreeSet<DefId>,
    production_contexts: BTreeSet<cull_core::ContextId>,
    test_values: BTreeSet<DefId>,
    test_contexts: BTreeSet<cull_core::ContextId>,
    external_values: BTreeSet<DefId>,
    external_contexts: BTreeSet<cull_core::ContextId>,
    module_modes: BTreeSet<(ReachabilityDomain, ModuleId, ModuleExecutionMode)>,
    graph_edges: BTreeMap<DefId, BTreeSet<DefId>>,
    dynamic_class_construction: BTreeSet<DefId>,
    worklist: BTreeSet<WorkItem>,
}

impl<'a, 'b, 'c> ReachabilityBuilder<'a, 'b, 'c> {
    fn analyze(
        graph: &'a SemanticGraph,
        project: &'a DiscoveredProject,
        parsed_modules: &'a [ParsedProjectModule],
        facts: &'a ProjectFacts<'a>,
        resolver: &'b mut ProjectResolver<'a, 'c>,
        mode: ProjectMode,
        diagnostics: &'b mut Vec<Diagnostic>,
    ) -> ReachabilityAnalysis {
        let mut builder = Self {
            project,
            graph,
            parsed_modules,
            facts,
            resolver,
            mode,
            diagnostics,
            roots: Vec::new(),
            production_values: BTreeSet::new(),
            production_contexts: BTreeSet::new(),
            test_values: BTreeSet::new(),
            test_contexts: BTreeSet::new(),
            external_values: BTreeSet::new(),
            external_contexts: BTreeSet::new(),
            module_modes: BTreeSet::new(),
            graph_edges: BTreeMap::new(),
            dynamic_class_construction: BTreeSet::new(),
            worklist: BTreeSet::new(),
        };

        builder.collect_static_graph_edges();
        builder.seed_roots();
        builder.solve();
        let root_coverage = builder.derive_root_coverage();

        ReachabilityAnalysis {
            root_coverage,
            roots: builder.roots,
            production_values: builder.production_values,
            production_contexts: builder.production_contexts,
            test_values: builder.test_values,
            test_contexts: builder.test_contexts,
            external_values: builder.external_values,
            external_contexts: builder.external_contexts,
            graph_edges: builder.graph_edges,
            dynamic_class_construction: builder.dynamic_class_construction,
        }
    }

    fn seed_roots(&mut self) {
        self.seed_configured_roots();
        self.seed_script_roots(false);
        self.seed_script_roots(true);
        self.seed_main_guard_roots();
        self.seed_package_main_roots();
        self.seed_test_roots();
        if self.mode != ProjectMode::Application {
            self.seed_external_surface_roots();
        }
    }

    fn collect_static_graph_edges(&mut self) {
        for definition in self.graph.definitions.clone() {
            let Some(parsed) = self.facts.module_source(definition.module) else {
                continue;
            };
            let Some(body) = definition_body(&parsed.syntax.body, &definition) else {
                continue;
            };
            self.collect_static_edges_from_statements(definition.module, definition.id, body);
        }
    }

    fn collect_static_edges_from_statements(
        &mut self,
        module: ModuleId,
        owner: DefId,
        statements: &[Stmt],
    ) {
        for statement in statements {
            match statement {
                Stmt::FunctionDef(function) => {
                    for decorator in &function.decorator_list {
                        self.collect_static_edges_from_expr(module, owner, &decorator.expression);
                    }
                    scan_function_definition_exprs(function, |expr| {
                        self.collect_static_edges_from_expr(module, owner, expr)
                    });
                }
                Stmt::ClassDef(class) => {
                    for decorator in &class.decorator_list {
                        self.collect_static_edges_from_expr(module, owner, &decorator.expression);
                    }
                    if let Some(arguments) = &class.arguments {
                        for arg in &arguments.args {
                            self.collect_static_edges_from_expr(module, owner, arg);
                        }
                        for keyword in &arguments.keywords {
                            self.collect_static_edges_from_expr(module, owner, &keyword.value);
                        }
                    }
                    self.collect_static_edges_from_statements(module, owner, &class.body);
                }
                Stmt::If(if_stmt) if is_type_checking_expr(&if_stmt.test) => {
                    for clause in &if_stmt.elif_else_clauses {
                        if clause.test.is_none() {
                            self.collect_static_edges_from_statements(module, owner, &clause.body);
                        }
                    }
                }
                Stmt::If(if_stmt) => {
                    self.collect_static_edges_from_expr(module, owner, &if_stmt.test);
                    self.collect_static_edges_from_statements(module, owner, &if_stmt.body);
                    for clause in &if_stmt.elif_else_clauses {
                        if let Some(test) = &clause.test {
                            self.collect_static_edges_from_expr(module, owner, test);
                        }
                        self.collect_static_edges_from_statements(module, owner, &clause.body);
                    }
                }
                _ => self.collect_static_edges_from_statement(module, owner, statement),
            }
        }
    }

    fn collect_static_edges_from_statement(
        &mut self,
        module: ModuleId,
        owner: DefId,
        statement: &Stmt,
    ) {
        match statement {
            Stmt::Assign(assign) => {
                self.collect_static_edges_from_expr(module, owner, &assign.value);
            }
            Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.collect_static_edges_from_expr(module, owner, value);
                }
                self.collect_static_edges_from_expr(module, owner, &assign.annotation);
            }
            Stmt::AugAssign(assign) => {
                self.collect_static_edges_from_expr(module, owner, &assign.value);
            }
            Stmt::Return(stmt) => {
                if let Some(value) = &stmt.value {
                    self.collect_static_edges_from_expr(module, owner, value);
                }
            }
            Stmt::Expr(expr) => self.collect_static_edges_from_expr(module, owner, &expr.value),
            Stmt::For(stmt) => {
                self.collect_static_edges_from_expr(module, owner, &stmt.iter);
                self.collect_static_edges_from_statements(module, owner, &stmt.body);
                self.collect_static_edges_from_statements(module, owner, &stmt.orelse);
            }
            Stmt::While(stmt) => {
                self.collect_static_edges_from_expr(module, owner, &stmt.test);
                self.collect_static_edges_from_statements(module, owner, &stmt.body);
                self.collect_static_edges_from_statements(module, owner, &stmt.orelse);
            }
            Stmt::With(stmt) => {
                for item in &stmt.items {
                    self.collect_static_edges_from_expr(module, owner, &item.context_expr);
                }
                self.collect_static_edges_from_statements(module, owner, &stmt.body);
            }
            Stmt::Try(stmt) => {
                self.collect_static_edges_from_statements(module, owner, &stmt.body);
                for handler in &stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(type_) = &handler.type_ {
                        self.collect_static_edges_from_expr(module, owner, type_);
                    }
                    self.collect_static_edges_from_statements(module, owner, &handler.body);
                }
                self.collect_static_edges_from_statements(module, owner, &stmt.orelse);
                self.collect_static_edges_from_statements(module, owner, &stmt.finalbody);
            }
            Stmt::Raise(stmt) => {
                if let Some(exc) = &stmt.exc {
                    self.collect_static_edges_from_expr(module, owner, exc);
                }
                if let Some(cause) = &stmt.cause {
                    self.collect_static_edges_from_expr(module, owner, cause);
                }
            }
            Stmt::Assert(stmt) => {
                self.collect_static_edges_from_expr(module, owner, &stmt.test);
                if let Some(msg) = &stmt.msg {
                    self.collect_static_edges_from_expr(module, owner, msg);
                }
            }
            _ => {}
        }
    }

    fn collect_static_edges_from_expr(
        &mut self,
        module: ModuleId,
        owner: DefId,
        expression: &Expr,
    ) {
        let value = self.resolver.resolve_expr_value_static(module, expression);
        for definition in value.definitions {
            self.graph_edges
                .entry(owner)
                .or_default()
                .insert(definition);
        }
        match expression {
            Expr::Attribute(attribute) => {
                self.collect_static_edges_from_expr(module, owner, &attribute.value);
            }
            Expr::Call(call) => {
                self.collect_static_edges_from_expr(module, owner, &call.func);
                for arg in &call.arguments.args {
                    self.collect_static_edges_from_expr(module, owner, arg);
                }
                for keyword in &call.arguments.keywords {
                    self.collect_static_edges_from_expr(module, owner, &keyword.value);
                }
            }
            Expr::BoolOp(expr) => {
                for value in &expr.values {
                    self.collect_static_edges_from_expr(module, owner, value);
                }
            }
            Expr::Named(expr) => self.collect_static_edges_from_expr(module, owner, &expr.value),
            Expr::BinOp(expr) => {
                self.collect_static_edges_from_expr(module, owner, &expr.left);
                self.collect_static_edges_from_expr(module, owner, &expr.right);
            }
            Expr::UnaryOp(expr) => {
                self.collect_static_edges_from_expr(module, owner, &expr.operand)
            }
            Expr::If(expr) => {
                self.collect_static_edges_from_expr(module, owner, &expr.test);
                self.collect_static_edges_from_expr(module, owner, &expr.body);
                self.collect_static_edges_from_expr(module, owner, &expr.orelse);
            }
            Expr::Compare(expr) => {
                self.collect_static_edges_from_expr(module, owner, &expr.left);
                for comparator in &expr.comparators {
                    self.collect_static_edges_from_expr(module, owner, comparator);
                }
            }
            Expr::Subscript(expr) => {
                self.collect_static_edges_from_expr(module, owner, &expr.value);
                self.collect_static_edges_from_expr(module, owner, &expr.slice);
            }
            Expr::List(expr) => {
                for element in &expr.elts {
                    self.collect_static_edges_from_expr(module, owner, element);
                }
            }
            Expr::Tuple(expr) => {
                for element in &expr.elts {
                    self.collect_static_edges_from_expr(module, owner, element);
                }
            }
            Expr::Set(expr) => {
                for element in &expr.elts {
                    self.collect_static_edges_from_expr(module, owner, element);
                }
            }
            Expr::Dict(expr) => {
                for item in &expr.items {
                    if let Some(key) = &item.key {
                        self.collect_static_edges_from_expr(module, owner, key);
                    }
                    self.collect_static_edges_from_expr(module, owner, &item.value);
                }
            }
            Expr::Starred(expr) => self.collect_static_edges_from_expr(module, owner, &expr.value),
            Expr::Slice(expr) => {
                if let Some(lower) = &expr.lower {
                    self.collect_static_edges_from_expr(module, owner, lower);
                }
                if let Some(upper) = &expr.upper {
                    self.collect_static_edges_from_expr(module, owner, upper);
                }
                if let Some(step) = &expr.step {
                    self.collect_static_edges_from_expr(module, owner, step);
                }
            }
            _ => {}
        }
    }

    fn seed_configured_roots(&mut self) {
        for selector in &self.project.configured_roots {
            let root_id = RootId::new(self.roots.len() as u32);
            if selector.is_module_root() {
                match self.resolver.namespace.resolve_absolute(&selector.module) {
                    LocalModuleResolution::Module(module) => {
                        self.roots.push(RootOutput {
                            id: root_id,
                            kind: RootKind::ConfiguredModule,
                            invocation: RootInvocation::ExecuteModule,
                            domain: ReachabilityDomain::Production,
                            target: selector.raw.clone(),
                            module: Some(self.facts.module_name(module)),
                            resolved: true,
                            detail: "configured module root executes as top-level code".to_owned(),
                        });
                        self.mark_module(
                            ReachabilityDomain::Production,
                            module,
                            ModuleExecutionMode::TopLevel,
                        );
                    }
                    _ => self.configured_root_error(
                        &selector.raw,
                        root_id,
                        RootKind::ConfiguredModule,
                    ),
                }
                continue;
            }

            match self.resolve_selector(selector) {
                Ok(definition) => {
                    let module_name = self.facts.definition(definition).map(|definition| {
                        self.mark_module(
                            ReachabilityDomain::Production,
                            definition.module,
                            ModuleExecutionMode::Imported,
                        );
                        self.facts.module_name(definition.module)
                    });
                    self.roots.push(RootOutput {
                        id: root_id,
                        kind: RootKind::ConfiguredObject,
                        invocation: RootInvocation::ExternalUse,
                        domain: ReachabilityDomain::Production,
                        target: selector.raw.clone(),
                        module: module_name,
                        resolved: true,
                        detail: "configured object root is treated as externally used".to_owned(),
                    });
                    self.activate_external_use(ReachabilityDomain::Production, definition);
                }
                Err(()) => {
                    self.configured_root_error(&selector.raw, root_id, RootKind::ConfiguredObject)
                }
            }
        }
    }

    fn configured_root_error(&mut self, raw: &str, root_id: RootId, kind: RootKind) {
        self.roots.push(RootOutput {
            id: root_id,
            kind,
            invocation: RootInvocation::ExternalUse,
            domain: ReachabilityDomain::Production,
            target: raw.to_owned(),
            module: None,
            resolved: false,
            detail: "configured root could not be resolved exactly".to_owned(),
        });
        self.diagnostics.push(Diagnostic::error(
            "CULL_P3001",
            format!("configured root `{raw}` could not be resolved exactly"),
        ));
    }

    fn seed_script_roots(&mut self, gui: bool) {
        let (scripts, kind) = if gui {
            (&self.project.gui_scripts, RootKind::GuiScript)
        } else {
            (&self.project.scripts, RootKind::ConsoleScript)
        };
        for script in scripts {
            let root_id = RootId::new(self.roots.len() as u32);
            match self.resolve_selector(&script.target) {
                Ok(definition) => {
                    let module_name = self.facts.definition(definition).map(|definition| {
                        self.mark_module(
                            ReachabilityDomain::Production,
                            definition.module,
                            ModuleExecutionMode::Imported,
                        );
                        self.facts.module_name(definition.module)
                    });
                    self.roots.push(RootOutput {
                        id: root_id,
                        kind,
                        invocation: RootInvocation::Call,
                        domain: ReachabilityDomain::Production,
                        target: format!("{}={}", script.name, script.target.raw),
                        module: module_name,
                        resolved: true,
                        detail: "project script wrapper imports and calls this target".to_owned(),
                    });
                    self.activate_callable(ReachabilityDomain::Production, definition);
                }
                Err(()) => {
                    self.roots.push(RootOutput {
                        id: root_id,
                        kind,
                        invocation: RootInvocation::Call,
                        domain: ReachabilityDomain::Production,
                        target: format!("{}={}", script.name, script.target.raw),
                        module: None,
                        resolved: false,
                        detail: "script target is valid metadata but not locally resolved"
                            .to_owned(),
                    });
                }
            }
        }
    }

    fn seed_main_guard_roots(&mut self) {
        let modules = self
            .parsed_modules
            .iter()
            .filter(|parsed| parsed.module.origin_domain == OriginDomain::Production)
            .filter(|parsed| module_has_main_guard(&parsed.syntax))
            .map(|parsed| parsed.module.id)
            .collect::<Vec<_>>();
        for module in modules {
            let root_id = RootId::new(self.roots.len() as u32);
            self.roots.push(RootOutput {
                id: root_id,
                kind: RootKind::MainGuard,
                invocation: RootInvocation::ExecuteModule,
                domain: ReachabilityDomain::Production,
                target: self.facts.module_name(module),
                module: Some(self.facts.module_name(module)),
                resolved: true,
                detail: "module contains a recognized __main__ guard".to_owned(),
            });
            self.mark_module(
                ReachabilityDomain::Production,
                module,
                ModuleExecutionMode::TopLevel,
            );
        }
    }

    fn seed_package_main_roots(&mut self) {
        let modules = self
            .parsed_modules
            .iter()
            .filter(|parsed| parsed.module.origin_domain == OriginDomain::Production)
            .filter(|parsed| parsed.module.name.ends_with(".__main__"))
            .map(|parsed| parsed.module.id)
            .collect::<Vec<_>>();
        for module in modules {
            let root_id = RootId::new(self.roots.len() as u32);
            self.roots.push(RootOutput {
                id: root_id,
                kind: RootKind::PackageMain,
                invocation: RootInvocation::ExecuteModule,
                domain: ReachabilityDomain::Production,
                target: self.facts.module_name(module),
                module: Some(self.facts.module_name(module)),
                resolved: true,
                detail: "local __main__.py can be executed with python -m".to_owned(),
            });
            self.mark_module(
                ReachabilityDomain::Production,
                module,
                ModuleExecutionMode::TopLevel,
            );
        }
    }

    fn seed_test_roots(&mut self) {
        let modules = self
            .parsed_modules
            .iter()
            .filter(|parsed| parsed.module.origin_domain == OriginDomain::Test)
            .map(|parsed| parsed.module.id)
            .collect::<Vec<_>>();
        for module in modules {
            let root_id = RootId::new(self.roots.len() as u32);
            self.roots.push(RootOutput {
                id: root_id,
                kind: RootKind::TestRoot,
                invocation: RootInvocation::ExecuteModule,
                domain: ReachabilityDomain::Test,
                target: self.facts.module_name(module),
                module: Some(self.facts.module_name(module)),
                resolved: true,
                detail: "test-origin module is analyzed in the isolated test domain".to_owned(),
            });
            self.mark_module(
                ReachabilityDomain::Test,
                module,
                ModuleExecutionMode::Imported,
            );
            for definition in self
                .graph
                .definitions
                .iter()
                .filter(|definition| definition.module == module && definition.reportable)
                .map(|definition| definition.id)
                .collect::<Vec<_>>()
            {
                self.activate_callable(ReachabilityDomain::Test, definition);
            }
        }
    }

    fn seed_external_surface_roots(&mut self) {
        let mut surface = BTreeSet::new();
        for export in &self.resolver.export_references {
            surface.insert(export.definition);
        }
        if self.mode == ProjectMode::Library {
            for definition in self
                .graph
                .definitions
                .iter()
                .filter(|definition| definition.reportable && is_public_name(&definition.name))
            {
                surface.insert(definition.id);
            }
        }
        for definition in surface {
            let Some(definition_fact) = self.facts.definition(definition) else {
                continue;
            };
            let root_id = RootId::new(self.roots.len() as u32);
            self.roots.push(RootOutput {
                id: root_id,
                kind: RootKind::LibrarySurface,
                invocation: RootInvocation::ExternalUse,
                domain: ReachabilityDomain::ExternalSurface,
                target: definition_fact.qualified_name.clone(),
                module: Some(self.facts.module_name(definition_fact.module)),
                resolved: true,
                detail: "public or exported surface is externally reachable in this mode"
                    .to_owned(),
            });
            self.mark_module(
                ReachabilityDomain::ExternalSurface,
                definition_fact.module,
                ModuleExecutionMode::Imported,
            );
            self.activate_external_use(ReachabilityDomain::ExternalSurface, definition);
        }
    }

    fn derive_root_coverage(&mut self) -> RootCoverage {
        if self.mode == ProjectMode::Library {
            return RootCoverage::NotApplicable;
        }
        let unresolved_declared = self
            .roots
            .iter()
            .any(|root| root.domain == ReachabilityDomain::Production && !root.resolved);
        let dynamic_unavailable = self.project.dynamic_scripts || self.project.dynamic_gui_scripts;
        let production_roots = self
            .roots
            .iter()
            .filter(|root| root.domain == ReachabilityDomain::Production && root.resolved)
            .count();
        match self.project.root_coverage {
            Some(crate::config::RootCoverageAssertion::Complete) => {
                if production_roots == 0 || unresolved_declared || dynamic_unavailable {
                    self.diagnostics.push(Diagnostic::error(
                        "CULL_P3002",
                        "root_coverage = \"complete\" was asserted, but Cull could not validate a complete production root set",
                    ));
                    RootCoverage::Partial
                } else {
                    RootCoverage::Complete
                }
            }
            Some(crate::config::RootCoverageAssertion::Partial) => {
                if production_roots == 0 && !unresolved_declared && !dynamic_unavailable {
                    RootCoverage::Absent
                } else {
                    RootCoverage::Partial
                }
            }
            None => {
                if production_roots == 0 && !unresolved_declared && !dynamic_unavailable {
                    RootCoverage::Absent
                } else {
                    RootCoverage::Partial
                }
            }
        }
    }

    fn solve(&mut self) {
        while let Some(item) = self.worklist.pop_first() {
            match item {
                WorkItem::Module {
                    domain,
                    module,
                    mode,
                } => self.scan_module(domain, module, mode),
                WorkItem::Context { domain, context } => self.scan_context(domain, context),
            }
        }
    }

    fn scan_module(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        mode: ModuleExecutionMode,
    ) {
        let Some(parsed) = self.facts.module_source(module) else {
            return;
        };
        let Some(context) = self.facts.module_context(module) else {
            return;
        };
        self.mark_context(domain, context);
        let mut locals = ScanLocals::default();
        self.scan_statements(
            domain,
            module,
            context,
            None,
            Some(mode),
            &parsed.syntax.body,
            &mut locals,
        );
    }

    fn scan_context(&mut self, domain: ReachabilityDomain, context: cull_core::ContextId) {
        let Some(context_fact) = self.graph.contexts.iter().find(|fact| fact.id == context) else {
            return;
        };
        let Some(owner) = context_fact.owner_definition else {
            return;
        };
        let Some(definition) = self.facts.definition(owner) else {
            return;
        };
        let Some(parsed) = self.facts.module_source(definition.module) else {
            return;
        };
        let Some(body) = definition_body(&parsed.syntax.body, definition) else {
            return;
        };
        let mut locals = ScanLocals::default();
        self.scan_statements(
            domain,
            definition.module,
            context,
            Some(owner),
            None,
            body,
            &mut locals,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn scan_statements(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        context: cull_core::ContextId,
        owner: Option<DefId>,
        module_mode: Option<ModuleExecutionMode>,
        statements: &[Stmt],
        locals: &mut ScanLocals,
    ) {
        for statement in statements {
            match statement {
                Stmt::Import(import) => self.scan_import(domain, module, import),
                Stmt::ImportFrom(import) => self.scan_import_from(domain, module, import),
                Stmt::FunctionDef(function) => {
                    let name_range = to_range(function.name.range);
                    let definition = self.facts.definition_for_name_range(
                        module,
                        name_range,
                        BindingKind::FunctionDefinition,
                    );
                    for decorator in &function.decorator_list {
                        self.scan_expr(
                            domain,
                            module,
                            owner,
                            &decorator.expression,
                            ExprUse::Call,
                            locals,
                        );
                    }
                    if let Some(definition) = definition {
                        if !function.decorator_list.is_empty() {
                            self.activate_callable(domain, definition);
                        }
                    }
                    scan_function_definition_exprs(function, |expr| {
                        self.scan_expr(domain, module, owner, expr, ExprUse::Runtime, locals)
                    });
                }
                Stmt::ClassDef(class) => {
                    let name_range = to_range(class.name.range);
                    let definition = self.facts.definition_for_name_range(
                        module,
                        name_range,
                        BindingKind::ClassDefinition,
                    );
                    for decorator in &class.decorator_list {
                        self.scan_expr(
                            domain,
                            module,
                            owner,
                            &decorator.expression,
                            ExprUse::Call,
                            locals,
                        );
                    }
                    if let Some(arguments) = &class.arguments {
                        for base in &arguments.args {
                            self.scan_expr(domain, module, owner, base, ExprUse::Runtime, locals);
                        }
                        for keyword in &arguments.keywords {
                            self.scan_expr(
                                domain,
                                module,
                                owner,
                                &keyword.value,
                                ExprUse::Runtime,
                                locals,
                            );
                        }
                    }
                    if let Some(definition) = definition {
                        if !class.decorator_list.is_empty() {
                            self.activate_external_use(domain, definition);
                        }
                        if let Some(class_def) = self.facts.definition(definition) {
                            self.mark_context(domain, class_def.context);
                            let mut class_locals = ScanLocals::default();
                            self.scan_statements(
                                domain,
                                module,
                                class_def.context,
                                Some(definition),
                                None,
                                &class.body,
                                &mut class_locals,
                            );
                        }
                    }
                }
                Stmt::Assign(assign) => {
                    self.scan_assignment(domain, module, owner, assign, locals);
                }
                Stmt::AnnAssign(assign) => {
                    if let Some(value) = &assign.value {
                        self.scan_expr(domain, module, owner, value, ExprUse::Runtime, locals);
                    }
                    if is_attribute_or_subscript_target(&assign.target) {
                        if let Some(value) = &assign.value {
                            self.scan_expr(domain, module, owner, value, ExprUse::Escape, locals);
                        }
                    }
                }
                Stmt::AugAssign(assign) => {
                    self.scan_expr(
                        domain,
                        module,
                        owner,
                        &assign.value,
                        ExprUse::Runtime,
                        locals,
                    );
                }
                Stmt::Return(stmt) => {
                    if let Some(value) = &stmt.value {
                        self.scan_expr(domain, module, owner, value, ExprUse::Escape, locals);
                    }
                }
                Stmt::Expr(expr) => {
                    self.scan_expr(domain, module, owner, &expr.value, ExprUse::Runtime, locals);
                }
                Stmt::If(if_stmt) => {
                    if let Some(mode) =
                        module_mode.and_then(|mode| main_guard_mode(&if_stmt.test, mode))
                    {
                        match mode {
                            MainGuardBranch::Body => self.scan_statements(
                                domain,
                                module,
                                context,
                                owner,
                                module_mode,
                                &if_stmt.body,
                                locals,
                            ),
                            MainGuardBranch::Else => {
                                for clause in &if_stmt.elif_else_clauses {
                                    if clause.test.is_none() {
                                        self.scan_statements(
                                            domain,
                                            module,
                                            context,
                                            owner,
                                            module_mode,
                                            &clause.body,
                                            locals,
                                        );
                                    }
                                }
                            }
                        }
                    } else if is_type_checking_expr(&if_stmt.test) {
                        for clause in &if_stmt.elif_else_clauses {
                            if clause.test.is_none() {
                                self.scan_statements(
                                    domain,
                                    module,
                                    context,
                                    owner,
                                    module_mode,
                                    &clause.body,
                                    locals,
                                );
                            }
                        }
                    } else {
                        self.scan_expr(
                            domain,
                            module,
                            owner,
                            &if_stmt.test,
                            ExprUse::Runtime,
                            locals,
                        );
                        self.scan_statements(
                            domain,
                            module,
                            context,
                            owner,
                            module_mode,
                            &if_stmt.body,
                            locals,
                        );
                        for clause in &if_stmt.elif_else_clauses {
                            if let Some(test) = &clause.test {
                                self.scan_expr(
                                    domain,
                                    module,
                                    owner,
                                    test,
                                    ExprUse::Runtime,
                                    locals,
                                );
                            }
                            self.scan_statements(
                                domain,
                                module,
                                context,
                                owner,
                                module_mode,
                                &clause.body,
                                locals,
                            );
                        }
                    }
                }
                _ => self.scan_statement_exprs(domain, module, context, owner, statement, locals),
            }
        }
    }

    fn scan_statement_exprs(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        context: cull_core::ContextId,
        owner: Option<DefId>,
        statement: &Stmt,
        locals: &mut ScanLocals,
    ) {
        match statement {
            Stmt::For(stmt) => {
                self.scan_expr(domain, module, owner, &stmt.iter, ExprUse::Runtime, locals);
                self.scan_statements(domain, module, context, owner, None, &stmt.body, locals);
                self.scan_statements(domain, module, context, owner, None, &stmt.orelse, locals);
            }
            Stmt::While(stmt) => {
                self.scan_expr(domain, module, owner, &stmt.test, ExprUse::Runtime, locals);
                self.scan_statements(domain, module, context, owner, None, &stmt.body, locals);
                self.scan_statements(domain, module, context, owner, None, &stmt.orelse, locals);
            }
            Stmt::With(stmt) => {
                for item in &stmt.items {
                    self.scan_expr(
                        domain,
                        module,
                        owner,
                        &item.context_expr,
                        ExprUse::Runtime,
                        locals,
                    );
                }
                self.scan_statements(domain, module, context, owner, None, &stmt.body, locals);
            }
            Stmt::Try(stmt) => {
                self.scan_statements(domain, module, context, owner, None, &stmt.body, locals);
                for handler in &stmt.handlers {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(type_) = &handler.type_ {
                        self.scan_expr(domain, module, owner, type_, ExprUse::Runtime, locals);
                    }
                    self.scan_statements(
                        domain,
                        module,
                        context,
                        owner,
                        None,
                        &handler.body,
                        locals,
                    );
                }
                self.scan_statements(domain, module, context, owner, None, &stmt.orelse, locals);
                self.scan_statements(
                    domain,
                    module,
                    context,
                    owner,
                    None,
                    &stmt.finalbody,
                    locals,
                );
            }
            Stmt::Raise(stmt) => {
                if let Some(exc) = &stmt.exc {
                    self.scan_expr(domain, module, owner, exc, ExprUse::Runtime, locals);
                }
                if let Some(cause) = &stmt.cause {
                    self.scan_expr(domain, module, owner, cause, ExprUse::Runtime, locals);
                }
            }
            Stmt::Assert(stmt) => {
                self.scan_expr(domain, module, owner, &stmt.test, ExprUse::Runtime, locals);
                if let Some(msg) = &stmt.msg {
                    self.scan_expr(domain, module, owner, msg, ExprUse::Runtime, locals);
                }
            }
            Stmt::Match(stmt) => {
                self.scan_expr(
                    domain,
                    module,
                    owner,
                    &stmt.subject,
                    ExprUse::Runtime,
                    locals,
                );
                for case in &stmt.cases {
                    if let Some(guard) = &case.guard {
                        self.scan_expr(domain, module, owner, guard, ExprUse::Runtime, locals);
                    }
                    self.scan_statements(domain, module, context, owner, None, &case.body, locals);
                }
            }
            _ => {}
        }
    }

    fn scan_assignment(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        owner: Option<DefId>,
        assign: &StmtAssign,
        locals: &mut ScanLocals,
    ) {
        self.scan_expr(
            domain,
            module,
            owner,
            &assign.value,
            ExprUse::Runtime,
            locals,
        );
        if let Some(classes) = self.call_class_targets(module, &assign.value) {
            for target in &assign.targets {
                if let Some(name) = target_name(target) {
                    locals
                        .instance_classes
                        .insert(name.to_owned(), classes.clone());
                }
            }
        }
        if assign.targets.iter().any(is_attribute_or_subscript_target) {
            self.scan_expr(
                domain,
                module,
                owner,
                &assign.value,
                ExprUse::Escape,
                locals,
            );
        }
    }

    fn scan_expr(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        owner: Option<DefId>,
        expression: &Expr,
        expr_use: ExprUse,
        locals: &mut ScanLocals,
    ) {
        match expression {
            Expr::Name(name) if matches!(name.ctx, ExprContext::Load) => {
                let value = self.resolver.resolve_expr_value_static(module, expression);
                self.mark_value_set(domain, owner, value, expr_use);
                if expr_use == ExprUse::Call {
                    if let Some(classes) = locals.instance_classes.get(name.id.as_str()).cloned() {
                        for class in classes {
                            self.activate_class_method(domain, class, "__call__");
                        }
                    }
                }
            }
            Expr::Attribute(attribute) => {
                if expr_use == ExprUse::Call {
                    if let Expr::Name(base) = &*attribute.value {
                        if let Some(classes) =
                            locals.instance_classes.get(base.id.as_str()).cloned()
                        {
                            for class in classes {
                                self.activate_class_method(
                                    domain,
                                    class,
                                    attribute.attr.id.as_str(),
                                );
                            }
                        }
                    }
                }
                let value = self.resolver.resolve_expr_value_static(module, expression);
                self.mark_value_set(domain, owner, value, expr_use);
                self.scan_expr(
                    domain,
                    module,
                    owner,
                    &attribute.value,
                    ExprUse::Runtime,
                    locals,
                );
            }
            Expr::Call(call) => {
                self.scan_expr(domain, module, owner, &call.func, ExprUse::Call, locals);
                for arg in &call.arguments.args {
                    self.scan_expr(domain, module, owner, arg, ExprUse::Escape, locals);
                }
                for keyword in &call.arguments.keywords {
                    self.scan_expr(
                        domain,
                        module,
                        owner,
                        &keyword.value,
                        ExprUse::Escape,
                        locals,
                    );
                }
            }
            Expr::List(expr) => {
                for element in &expr.elts {
                    self.scan_expr(domain, module, owner, element, ExprUse::Escape, locals);
                }
            }
            Expr::Tuple(expr) => {
                for element in &expr.elts {
                    self.scan_expr(domain, module, owner, element, ExprUse::Escape, locals);
                }
            }
            Expr::Set(expr) => {
                for element in &expr.elts {
                    self.scan_expr(domain, module, owner, element, ExprUse::Escape, locals);
                }
            }
            Expr::Dict(expr) => {
                for item in &expr.items {
                    if let Some(key) = &item.key {
                        self.scan_expr(domain, module, owner, key, ExprUse::Runtime, locals);
                    }
                    self.scan_expr(domain, module, owner, &item.value, ExprUse::Escape, locals);
                }
            }
            Expr::BoolOp(expr) => {
                for value in &expr.values {
                    self.scan_expr(domain, module, owner, value, expr_use, locals);
                }
            }
            Expr::Named(expr) => {
                self.scan_expr(domain, module, owner, &expr.value, expr_use, locals);
            }
            Expr::BinOp(expr) => {
                self.scan_expr(domain, module, owner, &expr.left, expr_use, locals);
                self.scan_expr(domain, module, owner, &expr.right, expr_use, locals);
            }
            Expr::UnaryOp(expr) => {
                self.scan_expr(domain, module, owner, &expr.operand, expr_use, locals)
            }
            Expr::If(expr) => {
                self.scan_expr(domain, module, owner, &expr.test, ExprUse::Runtime, locals);
                self.scan_expr(domain, module, owner, &expr.body, expr_use, locals);
                self.scan_expr(domain, module, owner, &expr.orelse, expr_use, locals);
            }
            Expr::Compare(expr) => {
                self.scan_expr(domain, module, owner, &expr.left, ExprUse::Runtime, locals);
                for comparator in &expr.comparators {
                    self.scan_expr(domain, module, owner, comparator, ExprUse::Runtime, locals);
                }
            }
            Expr::Subscript(expr) => {
                self.scan_expr(domain, module, owner, &expr.value, ExprUse::Runtime, locals);
                self.scan_expr(domain, module, owner, &expr.slice, ExprUse::Runtime, locals);
            }
            Expr::Starred(expr) => {
                self.scan_expr(domain, module, owner, &expr.value, expr_use, locals)
            }
            Expr::Slice(expr) => {
                if let Some(lower) = &expr.lower {
                    self.scan_expr(domain, module, owner, lower, ExprUse::Runtime, locals);
                }
                if let Some(upper) = &expr.upper {
                    self.scan_expr(domain, module, owner, upper, ExprUse::Runtime, locals);
                }
                if let Some(step) = &expr.step {
                    self.scan_expr(domain, module, owner, step, ExprUse::Runtime, locals);
                }
            }
            _ => {}
        }
    }

    fn scan_import(&mut self, domain: ReachabilityDomain, _module: ModuleId, import: &StmtImport) {
        for alias in &import.names {
            if let LocalModuleResolution::Module(target) = self
                .resolver
                .namespace
                .resolve_absolute(alias.name.id.as_str())
            {
                self.mark_module(domain, target, ModuleExecutionMode::Imported);
                continue;
            }
            if let Some(first) = alias.name.id.as_str().split('.').next() {
                if let LocalModuleResolution::Module(target) =
                    self.resolver.namespace.resolve_absolute(first)
                {
                    self.mark_module(domain, target, ModuleExecutionMode::Imported);
                }
            }
        }
    }

    fn scan_import_from(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        import: &StmtImportFrom,
    ) {
        let Some(base_name) = self.resolver.namespace.relative_module_name(
            module,
            import.level,
            import.module.as_ref().map(|module| module.id.as_str()),
        ) else {
            return;
        };
        if let LocalModuleResolution::Module(target) =
            self.resolver.namespace.resolve_absolute(&base_name)
        {
            self.mark_module(domain, target, ModuleExecutionMode::Imported);
        }
        for alias in &import.names {
            if alias.name.id.as_str() == "*" {
                continue;
            }
            let submodule = format!("{base_name}.{}", alias.name.id);
            if let LocalModuleResolution::Module(target) =
                self.resolver.namespace.resolve_absolute(&submodule)
            {
                self.mark_module(domain, target, ModuleExecutionMode::Imported);
            }
        }
    }

    fn mark_value_set(
        &mut self,
        domain: ReachabilityDomain,
        owner: Option<DefId>,
        value: ValueSet,
        expr_use: ExprUse,
    ) {
        for definition in value.definitions {
            if let Some(owner) = owner {
                self.graph_edges
                    .entry(owner)
                    .or_default()
                    .insert(definition);
            }
            self.mark_value(domain, definition);
            match expr_use {
                ExprUse::Runtime => {}
                ExprUse::Call => self.activate_callable(domain, definition),
                ExprUse::Escape => self.activate_external_use(domain, definition),
            }
        }
        for module in value.modules {
            self.mark_module(domain, module, ModuleExecutionMode::Imported);
        }
    }

    fn mark_module(
        &mut self,
        domain: ReachabilityDomain,
        module: ModuleId,
        mode: ModuleExecutionMode,
    ) {
        if self.module_modes.insert((domain, module, mode)) {
            self.worklist.insert(WorkItem::Module {
                domain,
                module,
                mode,
            });
        }
    }

    fn mark_value(&mut self, domain: ReachabilityDomain, definition: DefId) {
        match domain {
            ReachabilityDomain::Production => {
                self.production_values.insert(definition);
            }
            ReachabilityDomain::Test => {
                self.test_values.insert(definition);
            }
            ReachabilityDomain::ExternalSurface => {
                self.external_values.insert(definition);
            }
        }
    }

    fn mark_context(&mut self, domain: ReachabilityDomain, context: cull_core::ContextId) {
        let inserted = match domain {
            ReachabilityDomain::Production => self.production_contexts.insert(context),
            ReachabilityDomain::Test => self.test_contexts.insert(context),
            ReachabilityDomain::ExternalSurface => self.external_contexts.insert(context),
        };
        if inserted {
            self.worklist.insert(WorkItem::Context { domain, context });
        }
    }

    fn activate_callable(&mut self, domain: ReachabilityDomain, definition: DefId) {
        self.mark_value(domain, definition);
        let Some(definition_fact) = self.facts.definition(definition) else {
            return;
        };
        match definition_fact.kind {
            DefinitionKind::Function => self.mark_context(domain, definition_fact.context),
            DefinitionKind::Class => self.activate_class_construction(domain, definition),
        }
    }

    fn activate_external_use(&mut self, domain: ReachabilityDomain, definition: DefId) {
        self.mark_value(domain, definition);
        let Some(definition_fact) = self.facts.definition(definition) else {
            return;
        };
        match definition_fact.kind {
            DefinitionKind::Function => self.mark_context(domain, definition_fact.context),
            DefinitionKind::Class => self.activate_all_class_methods(domain, definition),
        }
    }

    fn activate_class_construction(&mut self, domain: ReachabilityDomain, definition: DefId) {
        self.mark_value(domain, definition);
        let metaclass = self.resolve_class_metaclass(definition);
        if let Some(metaclass) = metaclass {
            self.activate_class_method(domain, metaclass, "__call__");
        }
        self.activate_class_method(domain, definition, "__new__");
        self.activate_class_method(domain, definition, "__init__");
        if metaclass.is_none() && self.class_has_metaclass_keyword(definition) {
            self.dynamic_class_construction.insert(definition);
        }
    }

    fn activate_all_class_methods(&mut self, domain: ReachabilityDomain, class: DefId) {
        for method in self.class_methods(class) {
            self.mark_context(domain, method.context);
        }
    }

    fn activate_class_method(&mut self, domain: ReachabilityDomain, class: DefId, name: &str) {
        for method in self
            .class_methods(class)
            .into_iter()
            .filter(|method| method.name == name)
        {
            self.mark_context(domain, method.context);
        }
    }

    fn class_methods(&self, class: DefId) -> Vec<&'a SemanticDefinition> {
        class_methods_for(self.graph, self.facts, class)
    }

    fn call_class_targets(&self, module: ModuleId, expression: &Expr) -> Option<BTreeSet<DefId>> {
        let Expr::Call(call) = expression else {
            return None;
        };
        let value = self.resolver.resolve_expr_value_static(module, &call.func);
        let classes = value
            .definitions
            .into_iter()
            .filter(|definition| {
                self.facts
                    .definition(*definition)
                    .is_some_and(|definition| definition.kind == DefinitionKind::Class)
            })
            .collect::<BTreeSet<_>>();
        (!classes.is_empty()).then_some(classes)
    }

    fn resolve_class_metaclass(&self, class: DefId) -> Option<DefId> {
        let definition = self.facts.definition(class)?;
        let parsed = self.facts.module_source(definition.module)?;
        let class_stmt = class_statement(&parsed.syntax.body, definition)?;
        let arguments = class_stmt.arguments.as_ref()?;
        let keyword = arguments.keywords.iter().find(|keyword| {
            keyword
                .arg
                .as_ref()
                .is_some_and(|arg| arg.id.as_str() == "metaclass")
        })?;
        let value = self
            .resolver
            .resolve_expr_value_static(definition.module, &keyword.value);
        let definitions = value
            .definitions
            .into_iter()
            .filter(|definition| {
                self.facts
                    .definition(*definition)
                    .is_some_and(|definition| definition.kind == DefinitionKind::Class)
            })
            .collect::<BTreeSet<_>>();
        if definitions.len() == 1 {
            definitions.iter().next().copied()
        } else {
            None
        }
    }

    fn class_has_metaclass_keyword(&self, class: DefId) -> bool {
        let Some(definition) = self.facts.definition(class) else {
            return false;
        };
        let Some(parsed) = self.facts.module_source(definition.module) else {
            return false;
        };
        let Some(class_stmt) = class_statement(&parsed.syntax.body, definition) else {
            return false;
        };
        class_stmt.arguments.as_ref().is_some_and(|arguments| {
            arguments.keywords.iter().any(|keyword| {
                keyword
                    .arg
                    .as_ref()
                    .is_some_and(|arg| arg.id.as_str() == "metaclass")
            })
        })
    }

    fn resolve_selector(&mut self, selector: &crate::config::RootSelector) -> Result<DefId, ()> {
        let LocalModuleResolution::Module(module) =
            self.resolver.namespace.resolve_absolute(&selector.module)
        else {
            return Err(());
        };
        let Some((first, rest)) = selector.attributes.split_first() else {
            return Err(());
        };
        let mut definitions = self
            .resolver
            .module_slot_value(module, first)
            .definitions
            .into_iter()
            .collect::<BTreeSet<_>>();
        for attr in rest {
            let mut next = BTreeSet::new();
            for definition in definitions {
                let Some(definition_fact) = self.facts.definition(definition) else {
                    continue;
                };
                for candidate in self.graph.definitions.iter().filter(|candidate| {
                    candidate.module == definition_fact.module
                        && candidate.name == *attr
                        && self
                            .facts
                            .bindings
                            .get(&candidate.binding)
                            .is_some_and(|binding| binding.scope == definition_fact.scope)
                }) {
                    next.insert(candidate.id);
                }
            }
            definitions = next;
        }
        (definitions.len() == 1)
            .then(|| definitions.iter().next().copied())
            .flatten()
            .ok_or(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainGuardBranch {
    Body,
    Else,
}

fn module_has_main_guard(module: &ModModule) -> bool {
    module.body.iter().any(|statement| {
        matches!(
            statement,
            Stmt::If(if_stmt) if is_main_guard_equality(&if_stmt.test).is_some()
        )
    })
}

fn main_guard_mode(expression: &Expr, mode: ModuleExecutionMode) -> Option<MainGuardBranch> {
    let expects_top_level = is_main_guard_equality(expression)?;
    Some(match (expects_top_level, mode) {
        (true, ModuleExecutionMode::TopLevel) | (false, ModuleExecutionMode::Imported) => {
            MainGuardBranch::Body
        }
        (true, ModuleExecutionMode::Imported) | (false, ModuleExecutionMode::TopLevel) => {
            MainGuardBranch::Else
        }
    })
}

fn is_main_guard_equality(expression: &Expr) -> Option<bool> {
    let Expr::Compare(compare) = expression else {
        return None;
    };
    if compare.ops.len() != 1 || compare.comparators.len() != 1 {
        return None;
    }
    let left_name = is_dunder_name_expr(&compare.left);
    let left_main = is_main_literal_expr(&compare.left);
    let right_name = is_dunder_name_expr(&compare.comparators[0]);
    let right_main = is_main_literal_expr(&compare.comparators[0]);
    if !((left_name && right_main) || (left_main && right_name)) {
        return None;
    }
    match compare.ops[0] {
        CmpOp::Eq => Some(true),
        CmpOp::NotEq => Some(false),
        _ => None,
    }
}

fn is_dunder_name_expr(expression: &Expr) -> bool {
    matches!(expression, Expr::Name(name) if name.id.as_str() == "__name__")
}

fn is_main_literal_expr(expression: &Expr) -> bool {
    matches!(expression, Expr::StringLiteral(literal) if literal.value.to_str() == "__main__")
}

fn definition_body<'a>(
    statements: &'a [Stmt],
    definition: &SemanticDefinition,
) -> Option<&'a [Stmt]> {
    for statement in statements {
        match statement {
            Stmt::FunctionDef(function)
                if to_range(function.name.range) == definition.name_range =>
            {
                return Some(&function.body);
            }
            Stmt::FunctionDef(function) => {
                if let Some(body) = definition_body(&function.body, definition) {
                    return Some(body);
                }
            }
            Stmt::ClassDef(class) => {
                if to_range(class.name.range) == definition.name_range {
                    return Some(&class.body);
                }
                if let Some(body) = definition_body(&class.body, definition) {
                    return Some(body);
                }
            }
            Stmt::If(if_stmt) => {
                if let Some(body) = definition_body(&if_stmt.body, definition) {
                    return Some(body);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(body) = definition_body(&clause.body, definition) {
                        return Some(body);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn class_statement<'a>(
    statements: &'a [Stmt],
    definition: &SemanticDefinition,
) -> Option<&'a ruff_python_ast::StmtClassDef> {
    for statement in statements {
        match statement {
            Stmt::FunctionDef(function) => {
                if let Some(class) = class_statement(&function.body, definition) {
                    return Some(class);
                }
            }
            Stmt::ClassDef(class) if to_range(class.name.range) == definition.name_range => {
                return Some(class);
            }
            Stmt::ClassDef(class) => {
                if let Some(nested) = class_statement(&class.body, definition) {
                    return Some(nested);
                }
            }
            Stmt::If(if_stmt) => {
                if let Some(class) = class_statement(&if_stmt.body, definition) {
                    return Some(class);
                }
                for clause in &if_stmt.elif_else_clauses {
                    if let Some(class) = class_statement(&clause.body, definition) {
                        return Some(class);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn scan_function_definition_exprs<'a>(
    function: &'a ruff_python_ast::StmtFunctionDef,
    mut scan: impl FnMut(&'a Expr),
) {
    for parameter in function.parameters.iter_source_order() {
        if let Some(default) = parameter.default() {
            scan(default);
        }
        if let Some(annotation) = parameter.annotation() {
            scan(annotation);
        }
    }
    if let Some(returns) = &function.returns {
        scan(returns);
    }
}

fn is_attribute_or_subscript_target(expression: &Expr) -> bool {
    match expression {
        Expr::Attribute(_) | Expr::Subscript(_) => true,
        Expr::Tuple(tuple) => tuple.elts.iter().any(is_attribute_or_subscript_target),
        Expr::List(list) => list.elts.iter().any(is_attribute_or_subscript_target),
        _ => false,
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
    reachability: &ReachabilityAnalysis,
    mode: ProjectMode,
    project_completeness: &ProjectCompleteness,
) -> Vec<Finding> {
    let same_module_refs = same_module_references(graph, facts);
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
    let dead_cluster_members = dead_cluster_members(graph, reachability, mode);
    let dynamic_class_construction =
        dynamic_class_construction_members(graph, facts, resolver, reachability);

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

        let same_refs = same_module_refs
            .get(&definition.id)
            .cloned()
            .unwrap_or_default();
        let cross_ref_count = cross_refs.get(&definition.id).map_or(0, Vec::len);
        let export_refs = exports_by_definition
            .get(&definition.id)
            .cloned()
            .unwrap_or_default();
        let has_inbound = !same_refs.is_empty() || cross_ref_count > 0 || !export_refs.is_empty();

        if is_effectively_reachable(definition, reachability, mode, surface) {
            continue;
        }

        let force_root_unreachable = dead_cluster_members.contains(&definition.id);
        let root_unreachable_enabled = match reachability.root_coverage {
            RootCoverage::Complete | RootCoverage::Partial => true,
            RootCoverage::Absent => false,
            RootCoverage::NotApplicable => mode == ProjectMode::Library,
        };
        let finding_type = if force_root_unreachable && root_unreachable_enabled {
            FindingType::RootUnreachable
        } else if !has_inbound {
            FindingType::Unreferenced
        } else if root_unreachable_enabled {
            FindingType::RootUnreachable
        } else {
            continue;
        };

        let (mut confidence, mut reason) =
            confidence_for_finding(mode, surface, finding_type, reachability.root_coverage);
        let mut blockers = Vec::new();
        let mut uncertainty = resolver
            .module_uncertainty
            .get(&definition.module)
            .into_iter()
            .flatten()
            .map(|uncertainty| FindingUncertainty {
                kind: uncertainty.output_kind(),
                affected_region: UncertaintyRegion::module(facts.module_name(definition.module)),
                effects: uncertainty.effects(),
                detail: uncertainty.detail().to_owned(),
            })
            .collect::<Vec<_>>();
        if dynamic_class_construction.contains(&definition.id) {
            uncertainty.push(FindingUncertainty {
                kind: FindingUncertaintyKind::DynamicClassConstruction,
                affected_region: UncertaintyRegion::definition(definition.qualified_name.clone()),
                effects: vec![
                    UncertaintyEffect::MayInvokeCallable,
                    UncertaintyEffect::MayIntroduceReference,
                ],
                detail:
                    "custom class construction may dispatch through unresolved metaclass behavior"
                        .to_owned(),
            });
        }
        if confidence == FindingConfidence::Review {
            blockers.push(FindingBlocker {
                kind: blocker_kind_for_reason(&reason),
                detail: reason.clone(),
            });
        }
        if confidence == FindingConfidence::High && !uncertainty.is_empty() {
            confidence = FindingConfidence::Review;
            reason = "analysis uncertainty prevents a high-confidence claim".to_owned();
            blockers.push(FindingBlocker {
                kind: FindingBlockerKind::AnalysisUncertainty,
                detail: reason.clone(),
            });
        }
        if project_completeness.status == cull_core::ProjectCompletenessStatus::Partial {
            if confidence == FindingConfidence::High {
                confidence = FindingConfidence::Review;
                reason = "partial project analysis caps confidence at Review".to_owned();
            }
            blockers.push(FindingBlocker {
                kind: FindingBlockerKind::PartialProjectAnalysis,
                detail: "included source files were skipped under explicit partial-analysis mode"
                    .to_owned(),
            });
        }

        let Some(parsed) = source_by_module.get(&definition.module) else {
            continue;
        };
        let (line, column) = line_column(&parsed.source_text, definition.name_range.start);
        let rule_id = match (definition.kind, finding_type) {
            (DefinitionKind::Function, FindingType::Unreferenced) => FindingRule::Cull001,
            (DefinitionKind::Class, FindingType::Unreferenced) => FindingRule::Cull002,
            (DefinitionKind::Function, FindingType::RootUnreachable) => FindingRule::Cull003,
            (DefinitionKind::Class, FindingType::RootUnreachable) => FindingRule::Cull004,
        };
        let effects = definition_effects(definition, facts);
        if matches!(
            definition.removal_risk,
            RemovalRisk::Review(_) | RemovalRisk::Unknown
        ) {
            blockers.push(FindingBlocker {
                kind: FindingBlockerKind::RemovalRisk,
                detail: "removal risk requires review even when unusedness evidence is strong"
                    .to_owned(),
            });
        }
        let inbound_references = same_refs
            .iter()
            .filter_map(|reference| finding_reference(reference, facts))
            .collect::<Vec<_>>();
        let exports = export_refs
            .iter()
            .map(|export| cull_core::FindingExport {
                public_name: export.public_name.clone(),
                kind: export.kind,
                source_module: facts.module_name(export.source_module),
            })
            .collect::<Vec<_>>();
        let origin_domains = origin_domain_summary(&inbound_references, definition.origin_domain);
        let reference_phases = reference_phase_summary(&inbound_references);
        let definition_output = FindingDefinition {
            kind: definition.kind,
            name: definition.name.clone(),
            qualified_name: definition.qualified_name.clone(),
            module: facts.module_name(definition.module),
            file: parsed.module.display_path.clone(),
            range: definition.range,
            line,
            column,
        };
        let finding_id = candidate_fingerprint(rule_id, &definition_output);
        let secondary_conditions = secondary_conditions_for(
            finding_type,
            has_inbound,
            root_unreachable_enabled,
            force_root_unreachable,
        );
        let reachability_output =
            reachability_for_finding(definition, reachability, mode, finding_type);
        let removal_risk = FindingRemovalRisk::from_semantic(&definition.removal_risk, &effects);
        let evidence = evidence_for(
            finding_type,
            &inbound_references,
            &exports,
            surface,
            &reachability_output,
            &blockers,
            &uncertainty,
            &removal_risk,
            &secondary_conditions,
            project_completeness,
            force_root_unreachable,
        );
        let explanation = explanation_for(
            definition,
            confidence,
            surface,
            finding_type,
            force_root_unreachable,
            reachability,
        );
        let finding = Finding {
            finding_id: finding_id.clone(),
            id: finding_id,
            rule_id,
            finding_type,
            definition: definition_output,
            status: CandidateStatus::Reported,
            confidence,
            confidence_ceiling: confidence,
            blockers,
            inbound_references,
            reachability: reachability_output,
            exports,
            mode_effect: FindingModeEffect {
                mode,
                surface,
                confidence_ceiling: confidence,
                reason,
            },
            uncertainty,
            origin_domains,
            reference_phases,
            removal_risk,
            secondary_conditions,
            evidence,
            explanation,
        };
        findings.push(finding);
    }

    sort_findings(&mut findings);
    dedupe_finding_ids(&mut findings);
    findings
}

fn sort_findings(findings: &mut [Finding]) {
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
}

fn same_module_references<'a>(
    graph: &'a SemanticGraph,
    facts: &ProjectFacts<'a>,
) -> BTreeMap<DefId, Vec<&'a ReferenceFact>> {
    let mut refs: BTreeMap<DefId, Vec<&ReferenceFact>> = BTreeMap::new();
    for reference in &graph.references {
        for binding in reaching_bindings(graph, facts, reference) {
            if let Some(definition) = facts.definitions_by_binding.get(&binding) {
                refs.entry(*definition).or_default().push(reference);
            }
        }
    }
    refs
}

fn dead_cluster_members(
    graph: &SemanticGraph,
    reachability: &ReachabilityAnalysis,
    mode: ProjectMode,
) -> BTreeSet<DefId> {
    let unreachable = graph
        .definitions
        .iter()
        .filter(|definition| {
            !is_definition_reachable_for_cluster(definition, reachability, mode)
                && definition.role != cull_core::DefinitionRole::OverloadDeclaration
        })
        .map(|definition| definition.id)
        .collect::<BTreeSet<_>>();
    let mut adjacency: BTreeMap<DefId, BTreeSet<DefId>> = BTreeMap::new();
    let mut self_cycles = BTreeSet::new();
    for (source, targets) in &reachability.graph_edges {
        if !unreachable.contains(source) {
            continue;
        }
        for target in targets {
            if !unreachable.contains(target) {
                continue;
            }
            adjacency.entry(*source).or_default().insert(*target);
            adjacency.entry(*target).or_default().insert(*source);
            if source == target {
                self_cycles.insert(*source);
            }
        }
    }

    let reportable = graph
        .definitions
        .iter()
        .filter(|definition| definition.reportable)
        .map(|definition| definition.id)
        .collect::<BTreeSet<_>>();
    let mut result = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for start in unreachable {
        if !seen.insert(start) {
            continue;
        }
        let mut stack = vec![start];
        let mut component = BTreeSet::new();
        while let Some(current) = stack.pop() {
            component.insert(current);
            for next in adjacency.get(&current).into_iter().flatten() {
                if seen.insert(*next) {
                    stack.push(*next);
                }
            }
        }
        let reportable_members = component
            .iter()
            .filter(|definition| reportable.contains(definition))
            .copied()
            .collect::<BTreeSet<_>>();
        let nontrivial = reportable_members.len() >= 2
            || reportable_members
                .iter()
                .any(|definition| self_cycles.contains(definition));
        if nontrivial {
            result.extend(reportable_members);
        }
    }
    result
}

fn dynamic_class_construction_members(
    graph: &SemanticGraph,
    facts: &ProjectFacts<'_>,
    resolver: &ProjectResolver<'_, '_>,
    reachability: &ReachabilityAnalysis,
) -> BTreeSet<DefId> {
    let mut seeds = reachability.dynamic_class_construction.clone();
    for definition in graph
        .definitions
        .iter()
        .filter(|definition| definition.kind == DefinitionKind::Class)
        .filter(|definition| reachability.production_values.contains(&definition.id))
    {
        if class_has_unresolved_metaclass(definition, facts, resolver) {
            seeds.insert(definition.id);
        }
    }

    let mut result = BTreeSet::new();
    for class in seeds {
        result.insert(class);
        let mut stack = class_methods_for(graph, facts, class)
            .into_iter()
            .map(|method| method.id)
            .collect::<Vec<_>>();
        while let Some(definition) = stack.pop() {
            if !result.insert(definition) {
                continue;
            }
            if let Some(targets) = reachability.graph_edges.get(&definition) {
                stack.extend(targets.iter().copied());
            }
        }
    }
    result
}

fn class_has_unresolved_metaclass(
    definition: &SemanticDefinition,
    facts: &ProjectFacts<'_>,
    resolver: &ProjectResolver<'_, '_>,
) -> bool {
    let Some(parsed) = facts.module_source(definition.module) else {
        return false;
    };
    let Some(class_stmt) = class_statement(&parsed.syntax.body, definition) else {
        return false;
    };
    let Some(arguments) = class_stmt.arguments.as_ref() else {
        return false;
    };
    let Some(keyword) = arguments.keywords.iter().find(|keyword| {
        keyword
            .arg
            .as_ref()
            .is_some_and(|arg| arg.id.as_str() == "metaclass")
    }) else {
        return false;
    };
    let metaclasses = resolver
        .resolve_expr_value_static(definition.module, &keyword.value)
        .definitions
        .into_iter()
        .filter(|definition| {
            facts
                .definition(*definition)
                .is_some_and(|definition| definition.kind == DefinitionKind::Class)
        })
        .collect::<BTreeSet<_>>();
    metaclasses.len() != 1
}

fn class_methods_for<'a>(
    graph: &'a SemanticGraph,
    facts: &ProjectFacts<'a>,
    class: DefId,
) -> Vec<&'a SemanticDefinition> {
    let Some(class_def) = facts.definition(class) else {
        return Vec::new();
    };
    graph
        .definitions
        .iter()
        .filter(|definition| definition.module == class_def.module)
        .filter(|definition| {
            facts
                .bindings
                .get(&definition.binding)
                .is_some_and(|binding| binding.scope == class_def.scope)
        })
        .collect()
}

fn is_definition_reachable_for_cluster(
    definition: &SemanticDefinition,
    reachability: &ReachabilityAnalysis,
    mode: ProjectMode,
) -> bool {
    let surface = definition_surface(
        definition,
        reachability.external_values.contains(&definition.id),
    );
    is_effectively_reachable(definition, reachability, mode, surface)
}

fn is_effectively_reachable(
    definition: &SemanticDefinition,
    reachability: &ReachabilityAnalysis,
    mode: ProjectMode,
    surface: DefinitionSurface,
) -> bool {
    if reachability.production_reachable(definition) {
        return true;
    }
    if mode != ProjectMode::Application && reachability.external_surface_reachable(definition) {
        return true;
    }
    if mode == ProjectMode::Library {
        return matches!(
            surface,
            DefinitionSurface::Exported | DefinitionSurface::Public
        );
    }
    false
}

fn finding_reference(
    reference: &ReferenceFact,
    facts: &ProjectFacts<'_>,
) -> Option<cull_core::FindingReference> {
    let parsed = facts.module_source(reference.module)?;
    Some(cull_core::FindingReference {
        kind: match reference.role {
            cull_core::ReferenceRole::Import => FindingReferenceKind::Import,
            cull_core::ReferenceRole::ModuleAttribute => FindingReferenceKind::ModuleAttribute,
            cull_core::ReferenceRole::Export => FindingReferenceKind::Export,
            _ => FindingReferenceKind::SameModule,
        },
        source_module: facts.module_name(reference.module),
        source: reference.source_spelling.clone(),
        file: parsed.module.display_path.clone(),
        range: reference.span,
        phase: reference.phase,
        origin_domain: reference.origin_domain,
    })
}

fn origin_domain_summary(
    references: &[cull_core::FindingReference],
    definition_origin: OriginDomain,
) -> Vec<FindingOriginSummary> {
    let mut counts = vec![FindingOriginSummary {
        origin_domain: definition_origin,
        count: 1,
    }];
    for reference in references {
        if let Some(summary) = counts
            .iter_mut()
            .find(|summary| summary.origin_domain == reference.origin_domain)
        {
            summary.count += 1;
        } else {
            counts.push(FindingOriginSummary {
                origin_domain: reference.origin_domain,
                count: 1,
            });
        }
    }
    counts.sort_by_key(|summary| origin_domain_order(summary.origin_domain));
    counts
}

fn reference_phase_summary(
    references: &[cull_core::FindingReference],
) -> Vec<cull_core::FindingPhaseSummary> {
    let mut counts = Vec::<cull_core::FindingPhaseSummary>::new();
    for reference in references {
        if let Some(summary) = counts
            .iter_mut()
            .find(|summary| summary.phase == reference.phase)
        {
            summary.count += 1;
        } else {
            counts.push(cull_core::FindingPhaseSummary {
                phase: reference.phase,
                count: 1,
            });
        }
    }
    counts.sort_by_key(|summary| reference_phase_order(summary.phase));
    counts
}

fn origin_domain_order(domain: OriginDomain) -> u8 {
    match domain {
        OriginDomain::Production => 0,
        OriginDomain::Test => 1,
        OriginDomain::Unknown => 2,
    }
}

fn reference_phase_order(phase: ReferencePhase) -> u8 {
    match phase {
        ReferencePhase::DefinitionTime => 0,
        ReferencePhase::BodyRuntime => 1,
        ReferencePhase::ImportTime => 2,
        ReferencePhase::ExportSurface => 3,
        ReferencePhase::Root => 4,
        ReferencePhase::TypeOnly => 5,
        ReferencePhase::LazyAnnotation => 6,
    }
}

fn reachability_for_finding(
    definition: &SemanticDefinition,
    reachability: &ReachabilityAnalysis,
    mode: ProjectMode,
    finding_type: FindingType,
) -> FindingReachability {
    let production_reachable = reachability.production_reachable(definition);
    let test_reachable = reachability.test_reachable(definition);
    let external_surface_reachable = reachability.external_surface_reachable(definition);
    let status = match finding_type {
        FindingType::Unreferenced if reachability.root_coverage == RootCoverage::Absent => {
            FindingReachabilityStatus::NotComputed
        }
        FindingType::Unreferenced if mode == ProjectMode::Library => {
            FindingReachabilityStatus::NotApplicable
        }
        _ => FindingReachabilityStatus::NoRuntimePath,
    };
    let mut summary =
        "no runtime path was found in Cull's static reachability graph from recognized roots"
            .to_owned();
    if test_reachable && !production_reachable {
        summary.push_str("; the definition is reachable only from test roots");
    }
    if external_surface_reachable && mode != ProjectMode::Application {
        summary.push_str("; the definition is protected by external-surface reachability");
    }
    FindingReachability {
        status,
        root_coverage: reachability.root_coverage,
        production_reachable,
        test_reachable,
        external_surface_reachable,
        roots_considered: reachability.roots_considered(),
        summary,
    }
}

fn reaching_bindings(
    graph: &SemanticGraph,
    facts: &ProjectFacts<'_>,
    reference: &ReferenceFact,
) -> Vec<BindingId> {
    let cull_core::ReferenceBindingState::Analyzed(state) = &reference.binding_state else {
        return Vec::new();
    };
    if state.reachability == LocalReachability::Unreachable {
        return Vec::new();
    }
    let mut bindings = graph
        .binding_sets
        .iter()
        .find(|set| set.id == state.bindings)
        .map(|set| set.bindings.clone())
        .unwrap_or_default();
    if bindings.is_empty() {
        if let cull_core::Resolution::Resolved(symbol) = reference.lexical_target {
            bindings.extend(
                facts
                    .bindings_by_symbol
                    .get(&symbol)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
    }
    bindings.sort();
    bindings.dedup();
    bindings
}

fn candidate_fingerprint(rule_id: FindingRule, definition: &FindingDefinition) -> String {
    let input = format!(
        "{}\0{}\0{}:{}\0{:?}\0{}\0{}",
        rule_id.code(),
        definition.file,
        definition.range.start,
        definition.range.end,
        definition.kind,
        definition.qualified_name,
        definition.module
    );
    let digest = blake3::hash(input.as_bytes());
    let encoded = BASE32_NOPAD.encode(&digest.as_bytes()[..16]);
    format!("{}-{encoded}", rule_id.code())
}

fn dedupe_finding_ids(findings: &mut [Finding]) {
    let mut seen = BTreeMap::<String, usize>::new();
    for finding in findings {
        let base = finding.finding_id.clone();
        let count = seen.entry(base.clone()).or_default();
        *count += 1;
        if *count > 1 {
            let unique_id = format!("{base}-{}", *count);
            finding.finding_id = unique_id.clone();
            finding.id = unique_id;
        }
    }
}

fn subject_fingerprint(definition: &FindingDefinition) -> String {
    let input = format!(
        "{}\0{}:{}\0{:?}\0{}\0{}",
        definition.file,
        definition.range.start,
        definition.range.end,
        definition.kind,
        definition.qualified_name,
        definition.module
    );
    let digest = blake3::hash(input.as_bytes());
    format!("SUBJECT-{}", BASE32_NOPAD.encode(&digest.as_bytes()[..16]))
}

fn blocker_kind_for_reason(reason: &str) -> FindingBlockerKind {
    if reason.contains("root coverage") || reason.contains("production roots") {
        FindingBlockerKind::RootCoverage
    } else if reason.contains("public")
        || reason.contains("dunder")
        || reason.contains("surface")
        || reason.contains("mode")
    {
        FindingBlockerKind::PublicSurfacePolicy
    } else {
        FindingBlockerKind::AnalysisUncertainty
    }
}

fn secondary_conditions_for(
    finding_type: FindingType,
    has_inbound: bool,
    root_unreachable_enabled: bool,
    force_root_unreachable: bool,
) -> Vec<SecondaryCondition> {
    match finding_type {
        FindingType::RootUnreachable if !has_inbound || force_root_unreachable => {
            vec![SecondaryCondition::AlsoUnreferenced]
        }
        FindingType::Unreferenced if root_unreachable_enabled => {
            vec![SecondaryCondition::AlsoRootUnreachable]
        }
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn evidence_for(
    finding_type: FindingType,
    inbound_references: &[cull_core::FindingReference],
    exports: &[cull_core::FindingExport],
    surface: DefinitionSurface,
    reachability: &FindingReachability,
    blockers: &[FindingBlocker],
    uncertainty: &[FindingUncertainty],
    removal_risk: &FindingRemovalRisk,
    secondary_conditions: &[SecondaryCondition],
    project_completeness: &ProjectCompleteness,
    dead_cluster_override: bool,
) -> Vec<EvidenceRecord> {
    let mut evidence = Vec::new();
    match finding_type {
        FindingType::Unreferenced => evidence.push(EvidenceRecord {
            kind: EvidenceKind::NoInboundReferences,
            summary: "no resolved inbound references were found".to_owned(),
        }),
        FindingType::RootUnreachable => evidence.push(EvidenceRecord {
            kind: EvidenceKind::ReachabilitySummary,
            summary: reachability.summary.clone(),
        }),
    }
    if !inbound_references.is_empty() {
        evidence.push(EvidenceRecord {
            kind: EvidenceKind::InboundReferenceSummary,
            summary: format!(
                "{} inbound reference(s) were resolved",
                inbound_references.len()
            ),
        });
    }
    evidence.push(EvidenceRecord {
        kind: EvidenceKind::ExportStatus,
        summary: if exports.is_empty() {
            "definition is not exported through a static public surface".to_owned()
        } else {
            format!(
                "definition has {} static export reference(s)",
                exports.len()
            )
        },
    });
    evidence.push(EvidenceRecord {
        kind: EvidenceKind::ModePolicy,
        summary: format!("definition surface is {surface:?}"),
    });
    evidence.push(EvidenceRecord {
        kind: EvidenceKind::RootCoverage,
        summary: format!("root coverage is {:?}", reachability.root_coverage),
    });
    if dead_cluster_override {
        evidence.push(EvidenceRecord {
            kind: EvidenceKind::DeadClusterMembership,
            summary: "dead-cluster priority selected root-unreachable as the primary finding"
                .to_owned(),
        });
    }
    for blocker in blockers {
        evidence.push(EvidenceRecord {
            kind: EvidenceKind::ConfidenceBlocker,
            summary: blocker.detail.clone(),
        });
    }
    for item in uncertainty {
        evidence.push(EvidenceRecord {
            kind: EvidenceKind::Uncertainty,
            summary: item.detail.clone(),
        });
    }
    if !secondary_conditions.is_empty() {
        evidence.push(EvidenceRecord {
            kind: EvidenceKind::SecondaryCondition,
            summary: format!("secondary conditions: {secondary_conditions:?}"),
        });
    }
    if project_completeness.status == cull_core::ProjectCompletenessStatus::Partial {
        evidence.push(EvidenceRecord {
            kind: EvidenceKind::ProjectCompleteness,
            summary: format!(
                "partial analysis skipped {} included source file(s)",
                project_completeness.skipped_files.len()
            ),
        });
    }
    evidence.push(EvidenceRecord {
        kind: EvidenceKind::RemovalRisk,
        summary: match removal_risk {
            FindingRemovalRisk::NoKnownDefinitionEffects => {
                "no known definition-time effects were recorded".to_owned()
            }
            FindingRemovalRisk::Review { .. } => {
                "definition-time effects require removal-risk review".to_owned()
            }
            FindingRemovalRisk::Unknown => "removal risk is unknown".to_owned(),
        },
    });
    evidence
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
            "special dunder definitions are at most Review".to_owned(),
        ),
        DefinitionSurface::Exported | DefinitionSurface::ModuleProtocolHook => (
            FindingConfidence::Review,
            "surface is not normally reportable".to_owned(),
        ),
    }
}

fn confidence_for_finding(
    mode: ProjectMode,
    surface: DefinitionSurface,
    finding_type: FindingType,
    root_coverage: RootCoverage,
) -> (FindingConfidence, String) {
    let (mut confidence, mut reason) = confidence_for_surface(mode, surface);
    if finding_type == FindingType::RootUnreachable {
        match root_coverage {
            RootCoverage::Complete => {}
            RootCoverage::Partial => {
                confidence = FindingConfidence::Review;
                reason = "partial root coverage prevents a high-confidence root-unreachable claim"
                    .to_owned();
            }
            RootCoverage::Absent => {
                confidence = FindingConfidence::Review;
                reason = "absent root coverage disables high-confidence root-unreachable claims"
                    .to_owned();
            }
            RootCoverage::NotApplicable if mode == ProjectMode::Library => {}
            RootCoverage::NotApplicable => {
                confidence = FindingConfidence::Review;
                reason = "root coverage is not applicable in this mode".to_owned();
            }
        }
        if mode == ProjectMode::Auto && root_coverage != RootCoverage::Complete {
            confidence = FindingConfidence::Review;
            reason =
                "auto mode requires complete production roots for high-confidence CULL003/CULL004"
                    .to_owned();
        }
    }
    (confidence, reason)
}

fn explanation_for(
    definition: &SemanticDefinition,
    confidence: FindingConfidence,
    surface: DefinitionSurface,
    finding_type: FindingType,
    dead_cluster_override: bool,
    reachability: &ReachabilityAnalysis,
) -> Vec<String> {
    let mut explanation = match finding_type {
        FindingType::Unreferenced => vec!["no resolved inbound references were found".to_owned()],
        FindingType::RootUnreachable => vec![
            "resolved inbound references exist or the definition is part of a dead cluster"
                .to_owned(),
            "no production runtime path was found from recognized roots".to_owned(),
        ],
    };
    explanation.push(format!("definition surface: {surface:?}"));
    explanation.push(format!("root coverage: {:?}", reachability.root_coverage));
    if reachability.test_reachable(definition) && !reachability.production_reachable(definition) {
        explanation.push("definition is reachable only from test roots".to_owned());
    }
    if dead_cluster_override {
        explanation.push(
            "dead-cluster priority classified this weak unreachable component as root-unreachable"
                .to_owned(),
        );
    }
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
        suppressed: findings
            .iter()
            .map(|finding| finding.secondary_conditions.len())
            .sum(),
    }
}

pub(crate) fn candidates_from_check(output: &CheckOutput) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for finding in &output.findings {
        let subject_id = subject_fingerprint(&finding.definition);
        candidates.push(Candidate {
            candidate_id: finding.finding_id.clone(),
            subject_id: subject_id.clone(),
            rule_id: finding.rule_id,
            finding_type: finding.finding_type,
            definition: finding.definition.clone(),
            status: CandidateStatus::Reported,
            confidence: Some(finding.confidence),
            confidence_ceiling: finding.confidence_ceiling,
            blockers: finding.blockers.clone(),
            suppression_reasons: Vec::new(),
            uncertainty: finding.uncertainty.clone(),
            evidence: finding.evidence.clone(),
            removal_risk: finding.removal_risk.clone(),
            secondary_conditions: finding.secondary_conditions.clone(),
            explanation: finding.explanation.clone(),
        });

        for condition in &finding.secondary_conditions {
            let (rule_id, finding_type) = secondary_rule_for(*condition, finding.definition.kind);
            let candidate_id = candidate_fingerprint(rule_id, &finding.definition);
            candidates.push(Candidate {
                candidate_id,
                subject_id: subject_id.clone(),
                rule_id,
                finding_type,
                definition: finding.definition.clone(),
                status: CandidateStatus::Suppressed,
                confidence: None,
                confidence_ceiling: finding.confidence,
                blockers: finding.blockers.clone(),
                suppression_reasons: vec![SuppressionReason {
                    kind: SuppressionReasonKind::NonPrimaryRuleAlternative,
                    detail:
                        "another rule alternative was selected as the public primary finding"
                            .to_owned(),
                }],
                uncertainty: finding.uncertainty.clone(),
                evidence: vec![EvidenceRecord {
                    kind: EvidenceKind::SecondaryCondition,
                    summary:
                        "suppressed because this rule is a non-primary alternative for the same definition"
                            .to_owned(),
                }],
                removal_risk: finding.removal_risk.clone(),
                secondary_conditions: Vec::new(),
                explanation: vec![
                    "another rule alternative was selected as the public primary finding"
                        .to_owned(),
                ],
            });
        }
    }
    sort_candidates(&mut candidates);
    dedupe_candidate_ids(&mut candidates);
    candidates
}

fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(|left, right| {
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
            .then(left.candidate_id.cmp(&right.candidate_id))
    });
}

fn dedupe_candidate_ids(candidates: &mut [Candidate]) {
    let mut seen = BTreeMap::<String, usize>::new();
    for candidate in candidates {
        let base = candidate.candidate_id.clone();
        let count = seen.entry(base.clone()).or_default();
        *count += 1;
        if *count > 1 {
            candidate.candidate_id = format!("{base}-{}", *count);
        }
    }
}

fn secondary_rule_for(
    condition: SecondaryCondition,
    kind: DefinitionKind,
) -> (FindingRule, FindingType) {
    match (condition, kind) {
        (SecondaryCondition::AlsoUnreferenced, DefinitionKind::Function) => {
            (FindingRule::Cull001, FindingType::Unreferenced)
        }
        (SecondaryCondition::AlsoUnreferenced, DefinitionKind::Class) => {
            (FindingRule::Cull002, FindingType::Unreferenced)
        }
        (SecondaryCondition::AlsoRootUnreachable, DefinitionKind::Function) => {
            (FindingRule::Cull003, FindingType::RootUnreachable)
        }
        (SecondaryCondition::AlsoRootUnreachable, DefinitionKind::Class) => {
            (FindingRule::Cull004, FindingType::RootUnreachable)
        }
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
    nth_string_arg(call, 0)
}

fn nth_string_arg(call: &ExprCall, index: usize) -> Option<String> {
    let value = call.arguments.args.get(index)?;
    string_literal_value(value)
}

fn namespace_mapping_call(expression: &Expr) -> Option<&ExprCall> {
    let Expr::Call(call) = expression else {
        return None;
    };
    if !call.arguments.args.is_empty() || !call.arguments.keywords.is_empty() {
        return None;
    }
    let Expr::Name(name) = &*call.func else {
        return None;
    };
    matches!(name.id.as_str(), "globals" | "locals" | "vars").then_some(call)
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
