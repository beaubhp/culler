use serde::{Deserialize, Serialize};

use crate::{
    BindingFact, BindingSetFact, ContextFact, ContextFlowStatusFact, DefinitionEffectKind,
    DefinitionKind, Diagnostic, FlowUncertaintySetFact, InternalCandidateFact, OriginDomain,
    OriginEvidence, OverloadGroupFact, PythonVersion, ReferenceFact, ReferencePhase, RemovalRisk,
    ScopeFact, SemanticDefinition, SymbolFact, TextRange,
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
    pub target_python: PythonVersion,
    pub project_root: String,
    pub source_roots: Vec<SourceRootOutput>,
    pub mode: ProjectMode,
    pub findings: Vec<Finding>,
    pub summary: CheckSummary,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CheckSummary {
    pub high_confidence: usize,
    pub review: usize,
    pub suppressed: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Finding {
    pub id: String,
    pub rule_id: FindingRule,
    pub finding_type: FindingType,
    pub definition: FindingDefinition,
    pub confidence: FindingConfidence,
    pub inbound_references: Vec<FindingReference>,
    pub reachability: FindingReachability,
    pub exports: Vec<FindingExport>,
    pub mode_effect: FindingModeEffect,
    pub uncertainty: Vec<FindingUncertainty>,
    pub origin_domains: Vec<FindingOriginSummary>,
    pub reference_phases: Vec<FindingPhaseSummary>,
    pub removal_risk: FindingRemovalRisk,
    pub explanation: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FindingRule {
    Cull001,
    Cull002,
}

impl FindingRule {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Cull001 => "CULL001",
            Self::Cull002 => "CULL002",
        }
    }

    pub const fn text_name(self) -> &'static str {
        match self {
            Self::Cull001 => "unreferenced-function",
            Self::Cull002 => "unreferenced-class",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingType {
    Unreferenced,
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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingReachabilityStatus {
    NotComputed,
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
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingUncertaintyKind {
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
