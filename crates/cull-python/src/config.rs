use std::{fs, path::Path};

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
        }
    }
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
}

pub fn load_project_config(project_root: &Path) -> Result<ProjectConfig, ConfigError> {
    let path = project_root.join("pyproject.toml");
    if !path.exists() {
        return Ok(ProjectConfig::empty());
    }

    let text = fs::read_to_string(path)?;
    let pyproject: PyProject = toml::from_str(&text)?;
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
        return Ok(ProjectConfig::empty());
    };
    let Some(cull) = tool.cull else {
        let mut config = ProjectConfig::empty();
        config.test_paths = pytest_test_paths;
        config.test_path_origin_evidence = pytest_test_path_origin_evidence;
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
    })
}

#[derive(Debug, Deserialize)]
struct PyProject {
    tool: Option<Tool>,
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
