use crate::{Config, Error, Result};
use lh_protocol::{Manifest, Router, Specialist, toml};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub manifest: Manifest,
}
#[derive(Debug, Clone, Default)]
pub struct Registry {
    pub plugins: BTreeMap<String, Entry>,
    pub specialists: BTreeMap<String, Specialist>,
    pub routers: BTreeMap<String, Router>,
}
impl Registry {
    pub fn discover(config: &Config, config_dir: &Path) -> Result<Self> {
        let mut paths: Vec<PathBuf> = std::env::var_os("LH_PLUGIN_PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        paths.extend(config.plugin_path.clone());
        paths.push(config_dir.join("plugins"));
        paths.push(PathBuf::from("plugins"));
        Self::from_paths(&paths)
    }
    /// A search entry may be a plugin directory or a directory of plugin directories.
    /// Canonical paths are deduplicated; different files declaring the same ID are errors.
    pub fn from_paths(paths: &[PathBuf]) -> Result<Self> {
        let mut files = BTreeSet::new();
        for path in paths {
            if !path.exists() {
                continue;
            }
            if path.join("plugin.toml").is_file() {
                files.insert(path.join("plugin.toml").canonicalize()?);
            } else {
                for entry in std::fs::read_dir(path)? {
                    let file = entry?.path().join("plugin.toml");
                    if file.is_file() {
                        files.insert(file.canonicalize()?);
                    }
                }
            }
        }
        let mut registry = Self::default();
        let mut ids = BTreeMap::new();
        for path in files {
            let manifest: Manifest = toml::from_str(&std::fs::read_to_string(&path)?)
                .map_err(|e| Error::new(-32602, format!("{}: {e}", path.display())))?;
            if manifest.plugin.protocol != 1 {
                return Err(Error::new(
                    -32602,
                    format!(
                        "{}: unsupported protocol {}",
                        path.display(),
                        manifest.plugin.protocol
                    ),
                ));
            }
            for id in std::iter::once(&manifest.plugin.id)
                .chain(manifest.specialist.iter().map(|s| &s.id))
                .chain(manifest.router.iter().map(|r| &r.id))
            {
                if id.is_empty() {
                    return Err(Error::new(-32602, format!("{}: empty id", path.display())));
                }
                if let Some(previous) = ids.insert(id.clone(), path.clone()) {
                    return Err(Error::new(
                        -32602,
                        format!(
                            "duplicate id '{id}' in {} and {}",
                            previous.display(),
                            path.display()
                        ),
                    ));
                }
            }
            for specialist in &manifest.specialist {
                if specialist.requires.iter().any(|m| {
                    !["text", "image", "audio"].contains(&m.as_str())
                        || !specialist.inputs.contains(m)
                }) || specialist.hints.iter().any(|h| {
                    h.pattern.trim().is_empty()
                        || !h.pattern.chars().any(char::is_alphanumeric)
                        || !h.weight.is_finite()
                        || h.weight <= 0.0
                }) {
                    return Err(Error::new(
                        -32602,
                        format!("{}: invalid specialist requires/hints", path.display()),
                    ));
                }
                for choice in specialist
                    .choices()
                    .map_err(|e| Error::new(-32602, format!("{}: {e}", path.display())))?
                {
                    validate_model(&choice.model.source, &path)?;
                }
                registry
                    .specialists
                    .insert(specialist.id.clone(), specialist.clone());
            }
            if let Some(router) = &manifest.router {
                if !["rules", "decision-model", "plugin"].contains(&router.strategy.as_str())
                    || !(0.0..=1.0).contains(&router.min_confidence)
                {
                    return Err(Error::new(
                        -32602,
                        format!("{}: invalid router strategy/confidence", path.display()),
                    ));
                }
                if let Some(model) = &router.model {
                    validate_model(&model.source, &path)?;
                }
                registry.routers.insert(router.id.clone(), router.clone());
            }
            registry
                .plugins
                .insert(manifest.plugin.id.clone(), Entry { path, manifest });
        }
        for specialist in registry.specialists.values() {
            for choice in specialist.choices().map_err(|e| Error::new(-32602, e))? {
                registry.backend(&choice.backend)?;
            }
        }
        for router in registry
            .routers
            .values()
            .filter(|r| r.strategy == "decision-model")
        {
            registry.backend(
                router
                    .backend
                    .as_deref()
                    .ok_or_else(|| Error::new(-32602, "decision-model router needs backend"))?,
            )?;
            if router.model.is_none() {
                return Err(Error::new(-32602, "decision-model router needs model"));
            }
        }
        Ok(registry)
    }
    pub fn backend(&self, id: &str) -> Result<&Entry> {
        self.plugins
            .get(id)
            .filter(|p| {
                p.manifest.run.is_some()
                    && p.manifest.plugin.provides.iter().any(|s| s == "backend")
            })
            .ok_or_else(|| {
                Error::new(
                    -32602,
                    format!("backend '{id}' not found or not executable"),
                )
            })
    }
}
fn validate_model(source: &str, path: &Path) -> Result<()> {
    let valid = source.strip_prefix("file:").is_some_and(|p| !p.is_empty())
        || source
            .strip_prefix("hf:")
            .is_some_and(|p| p.contains('/') && !p.ends_with(':'))
        || source
            .strip_prefix("freetoken:")
            .is_some_and(|p| !p.is_empty());
    if valid {
        Ok(())
    } else {
        Err(Error::new(
            -32602,
            format!("{}: invalid model.source '{source}'", path.display()),
        ))
    }
}
