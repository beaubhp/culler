use cull_core::{Diagnostic, ModuleIr, PythonVersion};

use crate::DecodedSource;

pub trait PythonFrontend {
    fn parse_module(&self, input: ParseInput<'_>) -> Result<ParsedModule, Vec<Diagnostic>>;
}

#[derive(Clone, Copy, Debug)]
pub struct ParseInput<'a> {
    pub file_id: cull_core::FileId,
    pub module_id: cull_core::ModuleId,
    pub module_name: &'a str,
    pub display_path: &'a str,
    pub source: &'a DecodedSource,
    pub target_python: PythonVersion,
}

#[derive(Clone, Debug)]
pub struct ParsedModule {
    pub module: ModuleIr,
    pub diagnostics: Vec<Diagnostic>,
}
