use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

impl TextRange {
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PythonVersion {
    pub major: u8,
    pub minor: u8,
}

impl PythonVersion {
    pub const PY310: Self = Self {
        major: 3,
        minor: 10,
    };
    pub const PY311: Self = Self {
        major: 3,
        minor: 11,
    };
    pub const PY312: Self = Self {
        major: 3,
        minor: 12,
    };
    pub const PY313: Self = Self {
        major: 3,
        minor: 13,
    };
    pub const PY314: Self = Self {
        major: 3,
        minor: 14,
    };
    pub const PY315: Self = Self {
        major: 3,
        minor: 15,
    };

    pub const fn is_candidate_semantic_version(self) -> bool {
        self.major == 3 && self.minor >= 10 && self.minor <= 14
    }

    pub const fn is_forward_canary(self) -> bool {
        self.major == 3 && self.minor == 15
    }
}

impl Default for PythonVersion {
    fn default() -> Self {
        Self::PY314
    }
}

impl fmt::Display for PythonVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FromStr for PythonVersion {
    type Err = PythonVersionParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.strip_prefix("py").unwrap_or(value);
        let value = value.strip_prefix("python").unwrap_or(value);
        let (major, minor) = value
            .split_once('.')
            .ok_or_else(|| PythonVersionParseError(value.to_owned()))?;
        let major = major
            .parse()
            .map_err(|_| PythonVersionParseError(value.to_owned()))?;
        let minor = minor
            .parse()
            .map_err(|_| PythonVersionParseError(value.to_owned()))?;
        Ok(Self { major, minor })
    }
}

#[derive(Debug, Error)]
#[error("invalid Python version `{0}`")]
pub struct PythonVersionParseError(String);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DecodedSourceInfo {
    pub encoding: String,
    pub had_utf8_bom: bool,
}
