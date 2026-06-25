use std::{collections::BTreeMap, fs, path::Path};

use cull_core::{OriginEvidence, ProjectMode, PythonVersion};
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectConfig {
    pub source_roots: Vec<String>,
    pub excludes: Vec<String>,
    pub target_python: Option<PythonVersion>,
    pub mode: ProjectMode,
    pub test_paths: Vec<String>,
    pub test_path_origin_evidence: Option<OriginEvidence>,
    pub roots: Vec<RootSelector>,
    pub root_coverage: Option<RootCoverageAssertion>,
    pub scripts: Vec<ProjectScript>,
    pub gui_scripts: Vec<ProjectScript>,
    pub dynamic_scripts: bool,
    pub dynamic_gui_scripts: bool,
    pub allow_partial: bool,
}

impl ProjectConfig {
    pub fn empty() -> Self {
        Self {
            source_roots: Vec::new(),
            excludes: Vec::new(),
            target_python: None,
            mode: ProjectMode::Auto,
            test_paths: Vec::new(),
            test_path_origin_evidence: None,
            roots: Vec::new(),
            root_coverage: None,
            scripts: Vec::new(),
            gui_scripts: Vec::new(),
            dynamic_scripts: false,
            dynamic_gui_scripts: false,
            allow_partial: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootCoverageAssertion {
    Complete,
    Partial,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootSelector {
    pub raw: String,
    pub module: String,
    pub attributes: Vec<String>,
}

impl RootSelector {
    pub fn is_module_root(&self) -> bool {
        self.attributes.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectScript {
    pub name: String,
    pub target: RootSelector,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceRootKind {
    Explicit,
    ToolCull,
    ConventionalSrc,
    FlatProject,
}

impl SourceRootKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::ToolCull => "tool-cull",
            Self::ConventionalSrc => "conventional-src",
            Self::FlatProject => "flat-project",
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read pyproject.toml: {0}")]
    Read(#[from] std::io::Error),
    #[error("failed to parse pyproject.toml: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid [tool.cull].mode `{0}`; expected auto, application, or library")]
    InvalidMode(String),
    #[error("invalid [tool.cull].root_coverage `{0}`; expected complete or partial")]
    InvalidRootCoverage(String),
    #[error("invalid root object reference `{0}`; expected module or module:object.attr")]
    InvalidRootReference(String),
    #[error("invalid project script object reference for `{name}`: `{target}`; expected module:object.attr")]
    InvalidScriptReference { name: String, target: String },
}

pub fn load_project_config(project_root: &Path) -> Result<ProjectConfig, ConfigError> {
    let path = project_root.join("pyproject.toml");
    if !path.exists() {
        return Ok(ProjectConfig::empty());
    }

    let text = fs::read_to_string(path)?;
    let pyproject: PyProject = toml::from_str(&text)?;
    let (scripts, gui_scripts, dynamic_scripts, dynamic_gui_scripts) =
        parse_project_metadata(pyproject.project.as_ref())?;
    let pytest_test_paths = pyproject
        .tool
        .as_ref()
        .and_then(|tool| tool.pytest.as_ref())
        .and_then(|pytest| pytest.ini_options.as_ref())
        .and_then(|options| options.testpaths.clone())
        .map(StringOrVec::into_vec)
        .unwrap_or_default();
    let pytest_test_path_origin_evidence =
        (!pytest_test_paths.is_empty()).then_some(OriginEvidence::PytestTestPath);

    let Some(tool) = pyproject.tool else {
        let mut config = ProjectConfig::empty();
        config.scripts = scripts;
        config.gui_scripts = gui_scripts;
        config.dynamic_scripts = dynamic_scripts;
        config.dynamic_gui_scripts = dynamic_gui_scripts;
        return Ok(config);
    };
    let Some(cull) = tool.cull else {
        let mut config = ProjectConfig::empty();
        config.test_paths = pytest_test_paths;
        config.test_path_origin_evidence = pytest_test_path_origin_evidence;
        config.scripts = scripts;
        config.gui_scripts = gui_scripts;
        config.dynamic_scripts = dynamic_scripts;
        config.dynamic_gui_scripts = dynamic_gui_scripts;
        return Ok(config);
    };
    let cull_test_paths = cull.tests.map(StringOrVec::into_vec);
    let (test_paths, test_path_origin_evidence) = if let Some(paths) = cull_test_paths {
        (paths, Some(OriginEvidence::CullConfiguration))
    } else {
        (pytest_test_paths, pytest_test_path_origin_evidence)
    };

    let mode = match cull.mode.as_deref() {
        Some(value) => {
            parse_mode(value).ok_or_else(|| ConfigError::InvalidMode(value.to_owned()))?
        }
        None => ProjectMode::Auto,
    };
    let root_coverage = match cull.root_coverage.as_deref() {
        Some(value) => Some(
            parse_root_coverage(value)
                .ok_or_else(|| ConfigError::InvalidRootCoverage(value.to_owned()))?,
        ),
        None => None,
    };
    let roots = cull
        .roots
        .unwrap_or_default()
        .into_iter()
        .map(|root| parse_root_selector(&root).ok_or(ConfigError::InvalidRootReference(root)))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ProjectConfig {
        source_roots: cull.src.map(StringOrVec::into_vec).unwrap_or_default(),
        excludes: cull.exclude.unwrap_or_default(),
        target_python: cull
            .target_python
            .or(cull.target_version)
            .and_then(|value| value.parse().ok()),
        mode,
        test_paths,
        test_path_origin_evidence,
        roots,
        root_coverage,
        scripts,
        gui_scripts,
        dynamic_scripts,
        dynamic_gui_scripts,
        allow_partial: cull.allow_partial.unwrap_or(false),
    })
}

#[derive(Debug, Deserialize)]
struct PyProject {
    project: Option<ProjectMetadata>,
    tool: Option<Tool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct ProjectMetadata {
    scripts: Option<BTreeMap<String, String>>,
    gui_scripts: Option<BTreeMap<String, String>>,
    dynamic: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Tool {
    cull: Option<ToolCull>,
    pytest: Option<ToolPytest>,
}

#[derive(Debug, Deserialize)]
struct ToolCull {
    src: Option<StringOrVec>,
    exclude: Option<Vec<String>>,
    target_python: Option<String>,
    target_version: Option<String>,
    tests: Option<StringOrVec>,
    mode: Option<String>,
    roots: Option<Vec<String>>,
    root_coverage: Option<String>,
    allow_partial: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ToolPytest {
    ini_options: Option<PytestIniOptions>,
}

#[derive(Debug, Deserialize)]
struct PytestIniOptions {
    testpaths: Option<StringOrVec>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

impl StringOrVec {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

fn parse_mode(value: &str) -> Option<ProjectMode> {
    match value {
        "auto" => Some(ProjectMode::Auto),
        "application" => Some(ProjectMode::Application),
        "library" => Some(ProjectMode::Library),
        _ => None,
    }
}

fn parse_root_coverage(value: &str) -> Option<RootCoverageAssertion> {
    match value {
        "complete" => Some(RootCoverageAssertion::Complete),
        "partial" => Some(RootCoverageAssertion::Partial),
        _ => None,
    }
}

fn parse_project_metadata(
    project: Option<&ProjectMetadata>,
) -> Result<(Vec<ProjectScript>, Vec<ProjectScript>, bool, bool), ConfigError> {
    let Some(project) = project else {
        return Ok((Vec::new(), Vec::new(), false, false));
    };
    let dynamic = project
        .dynamic
        .as_ref()
        .map(|values| values.iter().map(String::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let scripts = parse_scripts(project.scripts.as_ref())?;
    let gui_scripts = parse_scripts(project.gui_scripts.as_ref())?;
    let dynamic_scripts = dynamic.contains(&"scripts") && scripts.is_empty();
    let dynamic_gui_scripts = dynamic.contains(&"gui-scripts") && gui_scripts.is_empty();
    Ok((scripts, gui_scripts, dynamic_scripts, dynamic_gui_scripts))
}

fn parse_scripts(
    scripts: Option<&BTreeMap<String, String>>,
) -> Result<Vec<ProjectScript>, ConfigError> {
    let mut parsed = Vec::new();
    for (name, target) in scripts.into_iter().flatten() {
        let Some(selector) = parse_script_selector(target) else {
            return Err(ConfigError::InvalidScriptReference {
                name: name.clone(),
                target: target.clone(),
            });
        };
        parsed.push(ProjectScript {
            name: name.clone(),
            target: selector,
        });
    }
    Ok(parsed)
}

fn parse_script_selector(value: &str) -> Option<RootSelector> {
    let selector = parse_root_selector(value)?;
    (!selector.attributes.is_empty()).then_some(selector)
}

fn parse_root_selector(value: &str) -> Option<RootSelector> {
    if value.trim() != value || value.is_empty() {
        return None;
    }
    let colon_count = value.bytes().filter(|byte| *byte == b':').count();
    if colon_count > 1 {
        return None;
    }
    let (module, attributes) = if let Some((module, object)) = value.split_once(':') {
        if object.is_empty() {
            return None;
        }
        (module, object.split('.').collect::<Vec<_>>())
    } else {
        (value, Vec::new())
    };
    if !is_dotted_identifier(module) || attributes.iter().any(|part| !is_identifier(part)) {
        return None;
    }
    Some(RootSelector {
        raw: value.to_owned(),
        module: module.to_owned(),
        attributes: attributes.into_iter().map(str::to_owned).collect(),
    })
}

fn is_dotted_identifier(value: &str) -> bool {
    !value.is_empty() && value.split('.').all(is_identifier)
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}
