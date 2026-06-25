use serde::{Deserialize, Serialize};

use crate::{
    BindingFact, BindingSetFact, ContextFact, ContextFlowStatusFact, DefinitionEffectKind,
    DefinitionKind, Diagnostic, FlowUncertaintySetFact, InternalCandidateFact, OriginDomain,
    OriginEvidence, OverloadGroupFact, PythonVersion, ReferenceFact, ReferencePhase, RemovalRisk,
    RootId, ScopeFact, SemanticDefinition, SymbolFact, TextRange,
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

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMode {
    #[default]
    Auto,
    Application,
    Library,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckOutput {
    pub schema_version: u32,
    pub analysis: CheckAnalysis,
    pub project_completeness: ProjectCompleteness,
    pub target_python: PythonVersion,
    pub project_root: String,
    pub source_roots: Vec<SourceRootOutput>,
    pub mode: ProjectMode,
    pub root_coverage: RootCoverage,
    pub roots: Vec<RootOutput>,
    pub findings: Vec<Finding>,
    pub summary: CheckSummary,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckAnalysis {
    pub mode: ProjectMode,
    pub target_python: PythonVersion,
    pub root_coverage: RootCoverage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProjectCompleteness {
    pub status: ProjectCompletenessStatus,
    pub skipped_files: Vec<SkippedFile>,
    pub skipped_reasons: Vec<String>,
    pub confidence_ceiling: Option<FindingConfidence>,
}

impl ProjectCompleteness {
    pub fn complete() -> Self {
        Self {
            status: ProjectCompletenessStatus::Complete,
            skipped_files: Vec::new(),
            skipped_reasons: Vec::new(),
            confidence_ceiling: None,
        }
    }

    pub fn partial(skipped_files: Vec<SkippedFile>) -> Self {
        let mut skipped_reasons = skipped_files
            .iter()
            .map(|file| file.reason.clone())
            .collect::<Vec<_>>();
        skipped_reasons.sort();
        skipped_reasons.dedup();
        Self {
            status: ProjectCompletenessStatus::Partial,
            skipped_files,
            skipped_reasons,
            confidence_ceiling: Some(FindingConfidence::Review),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectCompletenessStatus {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SkippedFile {
    pub path: String,
    pub reason: String,
    pub diagnostic_code: String,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckSummary {
    pub high_confidence: usize,
    pub review: usize,
    pub suppressed: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Finding {
    pub finding_id: String,
    pub id: String,
    pub rule_id: FindingRule,
    pub finding_type: FindingType,
    pub definition: FindingDefinition,
    pub status: CandidateStatus,
    pub confidence: FindingConfidence,
    pub confidence_ceiling: FindingConfidence,
    pub blockers: Vec<FindingBlocker>,
    pub inbound_references: Vec<FindingReference>,
    pub reachability: FindingReachability,
    pub exports: Vec<FindingExport>,
    pub mode_effect: FindingModeEffect,
    pub uncertainty: Vec<FindingUncertainty>,
    pub origin_domains: Vec<FindingOriginSummary>,
    pub reference_phases: Vec<FindingPhaseSummary>,
    pub removal_risk: FindingRemovalRisk,
    pub secondary_conditions: Vec<SecondaryCondition>,
    pub evidence: Vec<EvidenceRecord>,
    pub explanation: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Candidate {
    pub candidate_id: String,
    pub subject_id: String,
    pub rule_id: FindingRule,
    pub finding_type: FindingType,
    pub definition: FindingDefinition,
    pub status: CandidateStatus,
    pub confidence: Option<FindingConfidence>,
    pub confidence_ceiling: FindingConfidence,
    pub blockers: Vec<FindingBlocker>,
    pub suppression_reasons: Vec<SuppressionReason>,
    pub uncertainty: Vec<FindingUncertainty>,
    pub evidence: Vec<EvidenceRecord>,
    pub removal_risk: FindingRemovalRisk,
    pub secondary_conditions: Vec<SecondaryCondition>,
    pub explanation: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStatus {
    Reported,
    Suppressed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FindingRule {
    Cull001,
    Cull002,
    Cull003,
    Cull004,
}

impl FindingRule {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Cull001 => "CULL001",
            Self::Cull002 => "CULL002",
            Self::Cull003 => "CULL003",
            Self::Cull004 => "CULL004",
        }
    }

    pub const fn text_name(self) -> &'static str {
        match self {
            Self::Cull001 => "unreferenced-function",
            Self::Cull002 => "unreferenced-class",
            Self::Cull003 => "unreachable-function",
            Self::Cull004 => "unreachable-class",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingType {
    Unreferenced,
    RootUnreachable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingBlocker {
    pub kind: FindingBlockerKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingBlockerKind {
    AnalysisUncertainty,
    PartialProjectAnalysis,
    PublicSurfacePolicy,
    RemovalRisk,
    RootCoverage,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SuppressionReason {
    pub kind: SuppressionReasonKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReasonKind {
    NonPrimaryRuleAlternative,
    CandidateConstructionInvalidated,
    UnsupportedFlow,
    UnboundedDynamicBehavior,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecondaryCondition {
    AlsoUnreferenced,
    AlsoRootUnreachable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidenceRecord {
    pub kind: EvidenceKind,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    NoInboundReferences,
    InboundReferenceSummary,
    ExportStatus,
    ModePolicy,
    RootCoverage,
    ReachabilitySummary,
    DeadClusterMembership,
    OriginSummary,
    DefinitionEffect,
    RemovalRisk,
    ProjectCompleteness,
    ConfidenceBlocker,
    Uncertainty,
    SecondaryCondition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingDefinition {
    pub kind: DefinitionKind,
    pub name: String,
    pub qualified_name: String,
    pub module: String,
    pub file: String,
    pub range: TextRange,
    pub line: u32,
    pub column: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingConfidence {
    High,
    Review,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingReference {
    pub kind: FindingReferenceKind,
    pub source_module: String,
    pub source: String,
    pub file: String,
    pub range: TextRange,
    pub phase: ReferencePhase,
    pub origin_domain: OriginDomain,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingReferenceKind {
    SameModule,
    Import,
    ModuleAttribute,
    Export,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingReachability {
    pub status: FindingReachabilityStatus,
    pub root_coverage: RootCoverage,
    pub production_reachable: bool,
    pub test_reachable: bool,
    pub external_surface_reachable: bool,
    pub roots_considered: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingReachabilityStatus {
    NotComputed,
    NoRuntimePath,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootCoverage {
    Complete,
    Partial,
    Absent,
    NotApplicable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RootOutput {
    pub id: RootId,
    pub kind: RootKind,
    pub invocation: RootInvocation,
    pub domain: ReachabilityDomain,
    pub target: String,
    pub module: Option<String>,
    pub resolved: bool,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    ConfiguredModule,
    ConfiguredObject,
    ConsoleScript,
    GuiScript,
    MainGuard,
    PackageMain,
    TestRoot,
    LibrarySurface,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootInvocation {
    ExecuteModule,
    ExternalUse,
    Call,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReachabilityDomain {
    Production,
    Test,
    ExternalSurface,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingExport {
    pub public_name: String,
    pub kind: FindingExportKind,
    pub source_module: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingExportKind {
    ExplicitAll,
    DirectReExport,
    AliasedReExport,
    PackagePublicBinding,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingModeEffect {
    pub mode: ProjectMode,
    pub surface: DefinitionSurface,
    pub confidence_ceiling: FindingConfidence,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionSurface {
    Exported,
    ModuleProtocolHook,
    SpecialDunder,
    Private,
    Public,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingUncertainty {
    pub kind: FindingUncertaintyKind,
    pub affected_region: UncertaintyRegion,
    pub effects: Vec<UncertaintyEffect>,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingUncertaintyKind {
    DynamicClassConstruction,
    DynamicExport,
    DynamicImport,
    DynamicModuleAttribute,
    ExternalImport,
    ImportResolution,
    ModuleGetattr,
    NamespaceOrder,
    PartialInitialization,
    PublicSurfacePolicy,
    RemovalRisk,
    UnsupportedExport,
    UnsupportedNamespace,
    DynamicAttributeRead,
    DynamicExecution,
    NamespaceMutation,
    RuntimeAnnotationIntrospection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UncertaintyRegion {
    pub kind: UncertaintyRegionKind,
    pub target: String,
}

impl UncertaintyRegion {
    pub fn module(target: impl Into<String>) -> Self {
        Self {
            kind: UncertaintyRegionKind::ModuleNamespace,
            target: target.into(),
        }
    }

    pub fn definition(target: impl Into<String>) -> Self {
        Self {
            kind: UncertaintyRegionKind::Definition,
            target: target.into(),
        }
    }

    pub fn project() -> Self {
        Self {
            kind: UncertaintyRegionKind::Project,
            target: "<project>".to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyRegionKind {
    Definition,
    ExecutionContext,
    ModuleNamespace,
    ExportSurface,
    ObjectValue,
    Project,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyEffect {
    MayReadAnyAttribute,
    MayMutateNamespace,
    MayIntroduceReference,
    MayInvokeCallable,
    MayAlterExports,
    MayAlterRoots,
    MayEvaluateAnnotations,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingOriginSummary {
    pub origin_domain: OriginDomain,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FindingPhaseSummary {
    pub phase: ReferencePhase,
    pub count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "risk")]
pub enum FindingRemovalRisk {
    NoKnownDefinitionEffects,
    Review { effects: Vec<DefinitionEffectKind> },
    Unknown,
}

impl FindingRemovalRisk {
    pub fn from_semantic(risk: &RemovalRisk, effects: &[DefinitionEffectKind]) -> Self {
        match risk {
            RemovalRisk::NoKnownDefinitionEffects => Self::NoKnownDefinitionEffects,
            RemovalRisk::Review(_) => Self::Review {
                effects: effects.to_vec(),
            },
            RemovalRisk::Unknown => Self::Unknown,
        }
    }
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
    pub origin_domain: OriginDomain,
    pub origin_evidence: OriginEvidence,
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
    pub definition_effect_sets: Vec<crate::DefinitionEffectSetFact>,
    pub overload_groups: Vec<OverloadGroupFact>,
    pub internal_candidates: Vec<InternalCandidateFact>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DebugCandidatesOutput {
    pub schema_version: u32,
    pub analysis: CheckAnalysis,
    pub project_root: String,
    pub source_roots: Vec<SourceRootOutput>,
    pub project_completeness: ProjectCompleteness,
    pub candidates: Vec<Candidate>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ExplainOutput {
    pub schema_version: u32,
    pub selector: String,
    pub analysis: CheckAnalysis,
    pub project_root: String,
    pub project_completeness: ProjectCompleteness,
    pub result: ExplainResult,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum ExplainResult {
    Found { candidate: Box<Candidate> },
    Ambiguous { candidates: Vec<CandidateSummary> },
    NotFound,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateSummary {
    pub candidate_id: String,
    pub rule_id: FindingRule,
    pub status: CandidateStatus,
    pub confidence: Option<FindingConfidence>,
    pub qualified_name: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
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
