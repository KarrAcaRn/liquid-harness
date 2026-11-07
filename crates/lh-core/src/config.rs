use crate::{Error, Result};
use lh_protocol::{json::Value, toml};
use serde::{Deserialize, Serialize};

/// All shortcuts can be disabled together for a decision-model baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CascadeConfig {
    pub enabled: bool,
    pub requires: bool,
    pub hints: bool,
    pub sticky: bool,
    pub cache: bool,
    pub single_candidate: bool,
    pub cache_capacity: usize,
    pub hint_threshold: f64,
}
impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            requires: true,
            hints: true,
            sticky: true,
            cache: true,
            single_candidate: true,
            cache_capacity: 256,
            hint_threshold: 1.0,
        }
    }
}
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub agent: crate::agent::AgentConfig,
    #[serde(skip)]
    pub directory: PathBuf,
    pub server_token_env: Option<String>,
    pub cascade: CascadeConfig,
    pub default_specialist: String,
    pub router: String,
    pub plugin_path: Vec<PathBuf>,
    pub plugins: BTreeMap<String, PluginConfig>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            agent: crate::agent::AgentConfig::default(),
            directory: Self::directory().unwrap_or_else(|_| PathBuf::from(".")),
            cascade: CascadeConfig::default(),
            server_token_env: None,
            default_specialist: "lfm3-text".into(),
            router: "rules".into(),
            plugin_path: vec![],
            plugins: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    pub enabled: Option<bool>,
    pub config: BTreeMap<String, Value>,
}

impl Config {
    pub fn directory() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(p).join("liquid-harness"));
        }
        std::env::var_os("HOME")
            .map(|p| PathBuf::from(p).join(".config/liquid-harness"))
            .ok_or_else(|| Error::new(-32602, "Neither XDG_CONFIG_HOME nor HOME is set"))
    }
    pub fn load(explicit: Option<&Path>) -> Result<(Self, PathBuf)> {
        let override_path = explicit
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("LH_CONFIG").map(PathBuf::from));
        let required = override_path.is_some();
        let path = match override_path {
            Some(p) => p,
            None => Self::directory()?.join("config.toml"),
        };
        let directory = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut config: Self = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| Error::new(-32602, format!("{}: {e}", path.display())))?,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(Error::new(-32602, format!("{}: {e}", path.display()))),
        };
        for p in &mut config.plugin_path {
            if p.is_relative() {
                *p = directory.join(&*p);
            }
        }
        config.directory = directory.clone();
        config.agent.validate()?;
        Ok((config, directory))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_config_with_plugin_overrides() {
        let c: Config =
            toml::from_str("router='mock-router'\n[plugins.mock.config]\narbitrary=8").unwrap();
        assert_eq!(c.plugins["mock"].config["arbitrary"], 8);
        assert!(toml::from_str::<Config>("rouetr='oops'").is_err());
        assert!(toml::from_str::<Config>("[plugins.mock]\nconfgi={}").is_err());
    }
}
