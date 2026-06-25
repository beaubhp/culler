use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use cull_core::{
    Diagnostic, FileId, ModuleId, OriginDomain, OriginEvidence, ProjectMode, PythonVersion,
    SourceRootOutput,
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use thiserror::Error;
use walkdir::{DirEntry, WalkDir};

use crate::{
    config::{
        load_project_config, ConfigError, ProjectScript, RootCoverageAssertion, RootSelector,
        SourceRootKind,
    },
    paths::{relative_slash_path, slash_path},
};

const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".svn",
    ".tox",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "env",
    "site-packages",
    "venv",
];

#[derive(Clone, Debug)]
pub struct DiscoveryOptions {
    pub project_root: PathBuf,
    pub explicit_source_roots: Vec<PathBuf>,
    pub target_python: Option<PythonVersion>,
}

#[derive(Clone, Debug)]
pub struct DiscoveredProject {
    pub project_root: PathBuf,
    pub target_python: PythonVersion,
    pub mode: ProjectMode,
    pub source_roots: Vec<SourceRoot>,
    pub modules: Vec<DiscoveredModule>,
    pub configured_roots: Vec<RootSelector>,
    pub root_coverage: Option<RootCoverageAssertion>,
    pub scripts: Vec<ProjectScript>,
    pub gui_scripts: Vec<ProjectScript>,
    pub dynamic_scripts: bool,
    pub dynamic_gui_scripts: bool,
    pub allow_partial: bool,
    pub diagnostics: Vec<Diagnostic>,
}

impl DiscoveredProject {
    pub fn source_root_output(&self) -> Vec<SourceRootOutput> {
        self.source_roots
            .iter()
            .map(|root| SourceRootOutput {
                path: relative_slash_path(&self.project_root, &root.path),
                kind: root.kind.as_str().to_owned(),
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct SourceRoot {
    pub path: PathBuf,
    pub kind: SourceRootKind,
}

#[derive(Clone, Debug)]
pub struct DiscoveredModule {
    pub id: ModuleId,
    pub file: FileId,
    pub name: String,
    pub path: PathBuf,
    pub display_path: String,
    pub origin_domain: OriginDomain,
    pub origin_evidence: OriginEvidence,
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("project root does not exist: {0}")]
    MissingRoot(String),
    #[error("project path is not a directory: {0}")]
    RootNotDirectory(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
}

pub fn discover_project(options: DiscoveryOptions) -> Result<DiscoveredProject, DiscoveryError> {
    if !options.project_root.exists() {
        return Err(DiscoveryError::MissingRoot(slash_path(
            &options.project_root,
        )));
    }
    if !options.project_root.is_dir() {
        return Err(DiscoveryError::RootNotDirectory(slash_path(
            &options.project_root,
        )));
    }

    let project_root = fs::canonicalize(&options.project_root).unwrap_or(options.project_root);
    let config = load_project_config(&project_root)?;
    let target_python = options
        .target_python
        .or(config.target_python)
        .unwrap_or_default();
    let source_roots = choose_source_roots(
        &project_root,
        &options.explicit_source_roots,
        &config.source_roots,
    );
    let exclude_set = build_exclude_set(&config.excludes);

    let mut diagnostics = Vec::new();
    let mut discovered = Vec::new();
    for source_root in &source_roots {
        if !source_root.path.exists() {
            diagnostics.push(
                Diagnostic::error(
                    "CULL_P0001",
                    format!(
                        "source root does not exist: {}",
                        slash_path(&source_root.path)
                    ),
                )
                .with_path(relative_slash_path(&project_root, &source_root.path)),
            );
            continue;
        }
        if !source_root.path.is_dir() {
            diagnostics.push(
                Diagnostic::error(
                    "CULL_P0002",
                    format!(
                        "source root is not a directory: {}",
                        slash_path(&source_root.path)
                    ),
                )
                .with_path(relative_slash_path(&project_root, &source_root.path)),
            );
            continue;
        }
        collect_modules(
            &project_root,
            source_root,
            exclude_set.as_ref(),
            &mut discovered,
            &mut diagnostics,
        );
    }

    discovered.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.display_path.cmp(&right.display_path))
    });

    detect_module_collisions(&discovered, &mut diagnostics);

    let modules = discovered
        .into_iter()
        .enumerate()
        .map(|(index, mut module)| {
            module.id = ModuleId::new(index as u32);
            module.file = FileId::new(index as u32);
            let (origin_domain, origin_evidence) = classify_origin(&module.display_path, &config);
            module.origin_domain = origin_domain;
            module.origin_evidence = origin_evidence;
            module
        })
        .collect();

    Ok(DiscoveredProject {
        project_root,
        target_python,
        mode: config.mode,
        source_roots,
        modules,
        configured_roots: config.roots,
        root_coverage: config.root_coverage,
        scripts: config.scripts,
        gui_scripts: config.gui_scripts,
        dynamic_scripts: config.dynamic_scripts,
        dynamic_gui_scripts: config.dynamic_gui_scripts,
        allow_partial: config.allow_partial,
        diagnostics,
    })
}

fn choose_source_roots(
    project_root: &Path,
    explicit: &[PathBuf],
    configured: &[String],
) -> Vec<SourceRoot> {
    if !explicit.is_empty() {
        return explicit
            .iter()
            .map(|path| SourceRoot {
                path: absolutize(project_root, path),
                kind: SourceRootKind::Explicit,
            })
            .collect();
    }

    if !configured.is_empty() {
        return configured
            .iter()
            .map(|path| SourceRoot {
                path: absolutize(project_root, Path::new(path)),
                kind: SourceRootKind::ToolCull,
            })
            .collect();
    }

    let src = project_root.join("src");
    if src.is_dir() {
        return vec![SourceRoot {
            path: src,
            kind: SourceRootKind::ConventionalSrc,
        }];
    }

    vec![SourceRoot {
        path: project_root.to_path_buf(),
        kind: SourceRootKind::FlatProject,
    }]
}

fn absolutize(project_root: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    fs::canonicalize(&path).unwrap_or(path)
}

fn build_exclude_set(excludes: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in excludes {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }
    builder.build().ok().filter(|set| !set.is_empty())
}

fn collect_modules(
    project_root: &Path,
    source_root: &SourceRoot,
    exclude_set: Option<&GlobSet>,
    modules: &mut Vec<DiscoveredModule>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let walker = WalkDir::new(&source_root.path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_visit_entry(project_root, entry, exclude_set));

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                diagnostics.push(Diagnostic::error("CULL_P0003", error.to_string()));
                continue;
            }
        };

        if !entry.file_type().is_file() || entry.file_type().is_symlink() {
            continue;
        }
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("py") {
            continue;
        }

        let Some(name) = module_name_for_path(&source_root.path, entry.path()) else {
            diagnostics.push(
                Diagnostic::error(
                    "CULL_P0004",
                    "could not derive module name from Python file path",
                )
                .with_path(relative_slash_path(project_root, entry.path())),
            );
            continue;
        };

        modules.push(DiscoveredModule {
            id: ModuleId::new(0),
            file: FileId::new(0),
            name,
            path: entry.path().to_path_buf(),
            display_path: relative_slash_path(project_root, entry.path()),
            origin_domain: OriginDomain::Production,
            origin_evidence: OriginEvidence::DefaultProduction,
        });
    }
}

fn classify_origin(
    display_path: &str,
    config: &crate::config::ProjectConfig,
) -> (OriginDomain, OriginEvidence) {
    for path in &config.test_paths {
        let normalized = path.trim_matches('/');
        if !normalized.is_empty()
            && (display_path == normalized || display_path.starts_with(&format!("{normalized}/")))
        {
            return (
                OriginDomain::Test,
                config
                    .test_path_origin_evidence
                    .unwrap_or(OriginEvidence::CullConfiguration),
            );
        }
    }

    if display_path.contains("/tests/") || display_path.starts_with("tests/") {
        return (OriginDomain::Test, OriginEvidence::TestsDirectory);
    }

    let filename = Path::new(display_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(display_path);
    if filename.starts_with("test_") || filename.ends_with("_test.py") {
        return (OriginDomain::Test, OriginEvidence::PytestFilenamePattern);
    }

    (OriginDomain::Production, OriginEvidence::DefaultProduction)
}

fn should_visit_entry(
    project_root: &Path,
    entry: &DirEntry,
    exclude_set: Option<&GlobSet>,
) -> bool {
    let file_name = entry.file_name().to_string_lossy();
    if entry.file_type().is_dir() && DEFAULT_EXCLUDED_DIRS.contains(&file_name.as_ref()) {
        return false;
    }

    if let Ok(relative) = entry.path().strip_prefix(project_root) {
        if exclude_set.is_some_and(|set| set.is_match(relative)) {
            return false;
        }
    }

    true
}

fn module_name_for_path(source_root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(source_root).ok()?;
    let mut parts = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let last = parts.last_mut()?;
    if !last.ends_with(".py") {
        return None;
    }

    let stem = last.strip_suffix(".py")?.to_owned();
    if stem == "__init__" {
        parts.pop();
    } else {
        *last = stem;
    }

    (!parts.is_empty()).then(|| parts.join("."))
}

fn detect_module_collisions(modules: &[DiscoveredModule], diagnostics: &mut Vec<Diagnostic>) {
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for module in modules {
        by_name
            .entry(&module.name)
            .or_default()
            .push(&module.display_path);
    }

    for (name, paths) in by_name {
        if paths.len() <= 1 {
            continue;
        }
        diagnostics.push(Diagnostic::warning(
            "CULL_P0005",
            format!(
                "multiple source files map to module `{name}`: {}",
                paths.join(", ")
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_package_modules() {
        let root = Path::new("/repo/src");
        assert_eq!(
            module_name_for_path(root, Path::new("/repo/src/acme/cache.py")),
            Some("acme.cache".to_owned())
        );
        assert_eq!(
            module_name_for_path(root, Path::new("/repo/src/acme/__init__.py")),
            Some("acme".to_owned())
        );
    }
}
