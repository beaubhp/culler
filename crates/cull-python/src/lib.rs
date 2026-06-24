mod analysis;
mod check;
mod config;
mod decode;
mod discovery;
mod flow_analysis;
mod frontend;
mod module_namespace;
mod part1d_evidence;
mod paths;
mod ruff_frontend;
mod semantic_inventory;

pub use analysis::{
    analyze_check, analyze_debug_bindings, analyze_debug_definitions, analyze_debug_references,
    CheckOptions, DebugBindingsOptions, DebugDefinitionsOptions, DebugReferencesOptions,
};
pub use config::{ProjectConfig, SourceRootKind};
pub use decode::{decode_python_source, DecodedSource, SourceDecodeError};
pub use discovery::{discover_project, DiscoveredModule, DiscoveredProject};
pub use frontend::{ParsedModule, PythonFrontend};
pub use module_namespace::{
    LocalModuleResolution, ModuleNamespaceIndex, ModuleProviderFact, ModuleProviderKind,
    ModuleProviderStatus, NamespacePackageFact, PathEntryFact,
};
