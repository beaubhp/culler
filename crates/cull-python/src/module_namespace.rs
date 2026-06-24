use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use cull_core::ModuleId;
use serde::{Deserialize, Serialize};

use crate::{
    discovery::{DiscoveredModule, DiscoveredProject},
    paths::{relative_slash_path, slash_path},
};

#[derive(Clone, Debug)]
pub struct ModuleNamespaceIndex {
    pub source_roots: Vec<PathEntryFact>,
    pub providers: Vec<ModuleProviderFact>,
    pub selected_modules: BTreeMap<String, ModuleId>,
    pub namespace_packages: BTreeMap<String, NamespacePackageFact>,
    modules_by_path: BTreeMap<PathBuf, ModuleId>,
    modules_by_id: BTreeMap<ModuleId, DiscoveredModule>,
    source_root_paths: Vec<PathBuf>,
    project_root: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PathEntryFact {
    pub order: usize,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ModuleProviderFact {
    pub module_name: String,
    pub path: String,
    pub kind: ModuleProviderKind,
    pub status: ModuleProviderStatus,
    pub module: Option<ModuleId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleProviderKind {
    ModuleFile,
    RegularPackage,
    NamespacePackagePortion,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleProviderStatus {
    Selected,
    NamespaceContributor,
    Shadowed,
    Excluded,
    DuplicatePhysicalFile,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NamespacePackageFact {
    pub name: String,
    pub portions: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalModuleResolution {
    Module(ModuleId),
    Namespace(String),
    External,
    Unsupported(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SegmentResolution {
    Module(ModuleId),
    Package(ModuleId, PathBuf),
    Namespace(String, Vec<PathBuf>),
    External,
    Unsupported(String),
}

impl ModuleNamespaceIndex {
    pub fn build(project: &DiscoveredProject) -> Self {
        let source_root_paths = project
            .source_roots
            .iter()
            .map(|root| root.path.clone())
            .collect::<Vec<_>>();
        let mut index = Self {
            source_roots: source_root_paths
                .iter()
                .enumerate()
                .map(|(order, path)| PathEntryFact {
                    order,
                    path: relative_slash_path(&project.project_root, path),
                })
                .collect(),
            providers: Vec::new(),
            selected_modules: BTreeMap::new(),
            namespace_packages: BTreeMap::new(),
            modules_by_path: project
                .modules
                .iter()
                .map(|module| (module.path.clone(), module.id))
                .collect(),
            modules_by_id: project
                .modules
                .iter()
                .map(|module| (module.id, module.clone()))
                .collect(),
            source_root_paths,
            project_root: project.project_root.clone(),
        };

        let module_names = project
            .modules
            .iter()
            .map(|module| module.name.clone())
            .collect::<BTreeSet<_>>();
        for name in module_names {
            if let LocalModuleResolution::Module(module) = index.resolve_absolute(&name) {
                index.selected_modules.insert(name, module);
            }
        }

        let mut seen_physical = BTreeMap::<PathBuf, ModuleId>::new();
        for module in &project.modules {
            let kind = provider_kind(module);
            let status = if seen_physical
                .insert(module.path.clone(), module.id)
                .is_some()
            {
                ModuleProviderStatus::DuplicatePhysicalFile
            } else if index.selected_modules.get(&module.name) == Some(&module.id) {
                ModuleProviderStatus::Selected
            } else {
                ModuleProviderStatus::Shadowed
            };
            index.providers.push(ModuleProviderFact {
                module_name: module.name.clone(),
                path: module.display_path.clone(),
                kind,
                status,
                module: Some(module.id),
            });
        }

        for namespace in index.namespace_packages.values() {
            for portion in &namespace.portions {
                index.providers.push(ModuleProviderFact {
                    module_name: namespace.name.clone(),
                    path: portion.clone(),
                    kind: ModuleProviderKind::NamespacePackagePortion,
                    status: ModuleProviderStatus::NamespaceContributor,
                    module: None,
                });
            }
        }

        index.providers.sort_by(|left, right| {
            left.module_name
                .cmp(&right.module_name)
                .then(left.path.cmp(&right.path))
                .then_with(|| format!("{:?}", left.kind).cmp(&format!("{:?}", right.kind)))
        });
        index
    }

    pub fn module(&self, id: ModuleId) -> Option<&DiscoveredModule> {
        self.modules_by_id.get(&id)
    }

    pub fn module_for_name(&self, name: &str) -> Option<ModuleId> {
        self.selected_modules.get(name).copied()
    }

    pub fn resolve_absolute(&mut self, name: &str) -> LocalModuleResolution {
        if name.is_empty() {
            return LocalModuleResolution::External;
        }
        if let Some(module) = self.selected_modules.get(name) {
            return LocalModuleResolution::Module(*module);
        }
        if self.namespace_packages.contains_key(name) {
            return LocalModuleResolution::Namespace(name.to_owned());
        }

        let mut entries = self.source_root_paths.clone();
        let mut prefix = String::new();
        let mut last = None;
        for segment in name.split('.') {
            let next_prefix = if prefix.is_empty() {
                segment.to_owned()
            } else {
                format!("{prefix}.{segment}")
            };
            match self.resolve_segment(&entries, &next_prefix, segment) {
                SegmentResolution::Package(module, package_dir) => {
                    entries = vec![package_dir];
                    last = Some(LocalModuleResolution::Module(module));
                }
                SegmentResolution::Module(module) => {
                    last = Some(LocalModuleResolution::Module(module));
                    entries = Vec::new();
                }
                SegmentResolution::Namespace(namespace, portions) => {
                    entries = portions;
                    last = Some(LocalModuleResolution::Namespace(namespace));
                }
                SegmentResolution::External => return LocalModuleResolution::External,
                SegmentResolution::Unsupported(reason) => {
                    return LocalModuleResolution::Unsupported(reason);
                }
            }
            prefix = next_prefix;
        }
        last.unwrap_or(LocalModuleResolution::External)
    }

    fn resolve_segment(
        &mut self,
        entries: &[PathBuf],
        qualified_name: &str,
        segment: &str,
    ) -> SegmentResolution {
        let mut namespace_portions = Vec::new();
        for entry in entries {
            let package_dir = entry.join(segment);
            let package_init = package_dir.join("__init__.py");
            if package_init.exists() {
                return self
                    .modules_by_path
                    .get(&package_init)
                    .copied()
                    .map(|module| SegmentResolution::Package(module, package_dir))
                    .unwrap_or_else(|| {
                        SegmentResolution::Unsupported(format!(
                            "regular package provider is outside included modules: {}",
                            slash_path(&package_init)
                        ))
                    });
            }

            let module_file = entry.join(format!("{segment}.py"));
            if module_file.exists() {
                return self
                    .modules_by_path
                    .get(&module_file)
                    .copied()
                    .map(SegmentResolution::Module)
                    .unwrap_or_else(|| {
                        SegmentResolution::Unsupported(format!(
                            "module provider is outside included modules: {}",
                            slash_path(&module_file)
                        ))
                    });
            }

            if package_dir.is_dir() {
                namespace_portions.push(package_dir);
            }
        }

        if namespace_portions.is_empty() {
            return SegmentResolution::External;
        }

        let portions = namespace_portions
            .iter()
            .map(|path| relative_slash_path(&self.project_root, path))
            .collect::<Vec<_>>();
        self.namespace_packages
            .entry(qualified_name.to_owned())
            .or_insert_with(|| NamespacePackageFact {
                name: qualified_name.to_owned(),
                portions,
            });
        SegmentResolution::Namespace(qualified_name.to_owned(), namespace_portions)
    }

    pub fn relative_module_name(
        &mut self,
        current_module: ModuleId,
        level: u32,
        module: Option<&str>,
    ) -> Option<String> {
        let current = self.modules_by_id.get(&current_module)?;
        if level == 0 {
            return module.map(str::to_owned);
        }

        let mut parts = package_context_name(current).collect::<Vec<_>>();
        let climbs = level.saturating_sub(1) as usize;
        if climbs > parts.len() {
            return None;
        }
        for _ in 0..climbs {
            parts.pop();
        }
        if let Some(module) = module {
            parts.extend(module.split('.').map(str::to_owned));
        }
        (!parts.is_empty()).then(|| parts.join("."))
    }
}

fn provider_kind(module: &DiscoveredModule) -> ModuleProviderKind {
    if module.path.file_name().and_then(|name| name.to_str()) == Some("__init__.py") {
        ModuleProviderKind::RegularPackage
    } else {
        ModuleProviderKind::ModuleFile
    }
}

fn package_context_name(module: &DiscoveredModule) -> impl Iterator<Item = String> + '_ {
    let mut parts = module
        .name
        .split('.')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if module.path.file_name().and_then(|name| name.to_str()) != Some("__init__.py") {
        parts.pop();
    }
    parts.into_iter()
}

pub(crate) fn is_package_module(module: &DiscoveredModule) -> bool {
    module.path.file_name().and_then(|name| name.to_str()) == Some("__init__.py")
}

pub(crate) fn has_module_getattr(source_module: &ruff_python_ast::ModModule) -> bool {
    source_module.body.iter().any(|statement| {
        matches!(
            statement,
            ruff_python_ast::Stmt::FunctionDef(function) if function.name.id.as_str() == "__getattr__"
        )
    })
}
