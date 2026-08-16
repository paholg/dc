use std::path::{Path, PathBuf};

use eyre::{WrapErr, eyre};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::devcontainer::DevcontainerConfig;
use crate::helpers::{deserialize_shell_path, deserialize_shell_path_opt, validate_name};

pub(crate) const DEFAULT_PROXY_PORT: u16 = 43770;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ProjectName(String);

impl JsonSchema for ProjectName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ProjectName".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": r"^[A-Za-z0-9_-]+$",
        })
    }
}

impl ProjectName {
    pub(crate) fn new(s: String) -> Result<Self, String> {
        validate_name(&s)?;
        Ok(Self(s))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::ops::Deref for ProjectName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(|e| serde::de::Error::custom(format!("invalid project name: {e}")))
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) projects: IndexMap<ProjectName, Project>,
    #[serde(default)]
    pub(crate) proxy: ProxyGlobal,
    #[serde(default)]
    pub(crate) shell: ShellGlobal,
}

/// Global shell-integration settings, applied when you source
/// `COMPLETE=<shell> devconcurrent`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ShellGlobal {
    /// Also register a prompt hook that keeps the variables from
    /// `customizations.devconcurrent.env` in sync with the workspace you are
    /// standing in, as though you ran `dc show env --export` yourself.
    ///
    /// Default: false
    pub(crate) export_env: bool,
}

/// Global user proxy settings.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ProxyGlobal {
    /// The DNS port the proxy listens on.
    ///
    /// Default: 43770
    pub(crate) port: u16,
    /// Path to mkcert's CAROOT directory on the host. Required to serve
    /// services over https; leave unset to serve them over http only. Find it
    /// with `mkcert -CAROOT`.
    #[serde(default, deserialize_with = "deserialize_shell_path_opt")]
    pub(crate) ca_root: Option<PathBuf>,
}

impl Default for ProxyGlobal {
    fn default() -> Self {
        Self {
            port: DEFAULT_PROXY_PORT,
            ca_root: None,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Project {
    #[serde(deserialize_with = "deserialize_shell_path")]
    pub(crate) path: PathBuf,
    #[serde(default, deserialize_with = "deserialize_shell_path_opt")]
    pub(crate) worktree_folder: Option<PathBuf>,
    // We'll parse this properly when merging with Figment.
    #[schemars(with = "Option<DevcontainerConfig>")]
    pub(crate) devcontainer: Option<toml::Value>,
}

impl Config {
    pub(crate) fn load() -> eyre::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "devconcurrent")
            .ok_or_else(|| eyre::eyre!("could not determine config directory"))?;
        let path = dirs.config_dir().join("config.toml");
        Self::load_from_path(&path)
    }

    pub(crate) fn load_from_path(path: &Path) -> eyre::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("failed to load {}", path.display()))?;
        let de = toml::Deserializer::parse(&contents)
            .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
        serde_path_to_error::deserialize(de)
            .wrap_err_with(|| format!("failed to parse {}", path.display()))
    }

    pub(crate) fn project(
        &self,
        project_name: Option<String>,
    ) -> eyre::Result<(ProjectName, &Project)> {
        if let Some(name) = project_name.or_else(|| std::env::var("DC_PROJECT").ok()) {
            let name = ProjectName::new(name).map_err(|e| eyre!("invalid project name: {e}"))?;
            let project = self
                .projects
                .get(&name)
                .ok_or_else(|| eyre!("no project configured with name: {name:?}"))?;
            return Ok((name, project));
        }
        let repo_root = std::env::current_dir()
            .ok()
            .and_then(|cwd| repo_root_for(&cwd));
        if let Some(root) = repo_root
            && let Some(name) = self.project_name_for_repo_root(&root)?
        {
            let project = self
                .projects
                .get(&name)
                .expect("we just found this project");
            return Ok((name, project));
        }

        let (name, project) = self
            .projects
            .iter()
            .next()
            .ok_or_else(|| eyre!("no projects configured"))?;
        Ok((name.clone(), project))
    }

    fn project_name_for_repo_root(&self, repo_root: &Path) -> eyre::Result<Option<ProjectName>> {
        let canonical_root = repo_root.canonicalize()?;
        let name = self
            .projects
            .iter()
            .find(|(_, p)| {
                p.path == canonical_root || p.path.canonicalize().is_ok_and(|p| p == canonical_root)
            })
            .map(|(name, _)| name.clone());

        Ok(name)
    }
}

fn repo_root_for(cwd: &Path) -> Option<PathBuf> {
    let repo = gix::discover(cwd).ok()?;
    let main = repo.main_repo().ok()?;
    main.workdir().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn project_order_is_stable() {
        let names = [
            "zebra", "alpha", "mike", "bravo", "yankee", "charlie", "xray", "delta",
        ];
        let mut toml = String::new();
        for name in names {
            toml.push_str(&format!("[projects.{name}]\npath = \"/tmp/{name}\"\n\n"));
        }

        let mut file = tempfile::Builder::new().suffix(".toml").tempfile().unwrap();
        file.write_all(toml.as_bytes()).unwrap();

        let first = Config::load_from_path(file.path()).unwrap();
        let expected: Vec<&str> = first.projects.keys().map(ProjectName::as_str).collect();
        assert_eq!(expected, names);

        for i in 0..50 {
            let cfg = Config::load_from_path(file.path()).unwrap();
            let got: Vec<&str> = cfg.projects.keys().map(ProjectName::as_str).collect();
            assert_eq!(got, expected, "project order changed on iteration {i}");
        }
    }
}
