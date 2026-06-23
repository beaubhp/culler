use std::{fs, path::Path};

use cull_core::PythonVersion;
use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectConfig {
    pub source_roots: Vec<String>,
    pub excludes: Vec<String>,
    pub target_python: Option<PythonVersion>,
}

impl ProjectConfig {
    pub fn empty() -> Self {
        Self {
            source_roots: Vec::new(),
            excludes: Vec::new(),
            target_python: None,
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
}

pub fn load_project_config(project_root: &Path) -> Result<ProjectConfig, ConfigError> {
    let path = project_root.join("pyproject.toml");
    if !path.exists() {
        return Ok(ProjectConfig::empty());
    }

    let text = fs::read_to_string(path)?;
    let pyproject: PyProject = toml::from_str(&text)?;
    let Some(tool) = pyproject.tool else {
        return Ok(ProjectConfig::empty());
    };
    let Some(cull) = tool.cull else {
        return Ok(ProjectConfig::empty());
    };

    Ok(ProjectConfig {
        source_roots: cull.src.map(StringOrVec::into_vec).unwrap_or_default(),
        excludes: cull.exclude.unwrap_or_default(),
        target_python: cull
            .target_python
            .or(cull.target_version)
            .and_then(|value| value.parse().ok()),
    })
}

#[derive(Debug, Deserialize)]
struct PyProject {
    tool: Option<Tool>,
}

#[derive(Debug, Deserialize)]
struct Tool {
    cull: Option<ToolCull>,
}

#[derive(Debug, Deserialize)]
struct ToolCull {
    src: Option<StringOrVec>,
    exclude: Option<Vec<String>>,
    target_python: Option<String>,
    target_version: Option<String>,
}

#[derive(Debug, Deserialize)]
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
