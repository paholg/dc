use std::path::PathBuf;

use eyre::eyre;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_inline_default::serde_inline_default;

use crate::runner::cmd::Cmd;

fn deserialize_shell_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PathBuf::from(shellexpand::tilde(&s).as_ref()))
}

fn deserialize_shell_path_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PathBuf>, D::Error> {
    Option::<String>::deserialize(d).map(|o| o.map(|s| PathBuf::from(shellexpand::tilde(&s).as_ref())))
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_cmd: Option<Cmd>,

    #[serde(default)]
    pub projects: IndexMap<String, Project>,
}

#[serde_inline_default]
#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    #[serde(deserialize_with = "deserialize_shell_path")]
    pub path: PathBuf,

    #[serde(flatten)]
    pub options: ProjectOptions,
}

#[serde_inline_default]
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectOptions {
    #[serde(default)]
    pub default_cmd: Option<Cmd>,

    /// Directory to create workspaces in [default: /tmp/].
    #[serde(default, deserialize_with = "deserialize_shell_path_opt")]
    workspace_dir: Option<PathBuf>,

    /// If set, this port will be used automatically by the `dc fwd` command, to
    /// map a static host port to the container of your choice.
    pub fwd_port: Option<u16>,

    /// Port inside the container to forward to. Defaults to `fwd_port` if unset.
    pub container_port: Option<u16>,
}

impl ProjectOptions {
    pub fn workspace_dir(&self) -> PathBuf {
        self.workspace_dir.clone().unwrap_or("/tmp/".into())
    }

    fn apply_overrides(&mut self, overrides: ProjectOptions) {
        if overrides.default_cmd.is_some() {
            self.default_cmd = overrides.default_cmd;
        }
        if overrides.workspace_dir.is_some() {
            self.workspace_dir = overrides.workspace_dir;
        }
        if overrides.fwd_port.is_some() {
            self.fwd_port = overrides.fwd_port;
        }
        if overrides.container_port.is_some() {
            self.container_port = overrides.container_port;
        }
    }
}

impl Config {
    pub fn load() -> eyre::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "dc")
            .ok_or_else(|| eyre::eyre!("could not determine config directory"))?;
        let path = dirs.config_dir().join("config.toml");
        let cfg = config::Config::builder()
            .add_source(config::File::from(path))
            .build()?;
        Ok(cfg.try_deserialize()?)
    }

    pub fn project<'a>(&'a self, name: Option<&'a str>) -> eyre::Result<(&'a str, Project)> {
        let (name, base) = match name {
            Some(name) => self
                .projects
                .get(name)
                .map(|p| (name, p))
                .ok_or_else(|| eyre!("no project configured with name: {name}"))?,
            None => self
                .projects
                .iter()
                .next()
                .map(|(n, p)| (n.as_ref(), p))
                .ok_or_else(|| eyre!("no projects configured"))?,
        };

        let mut project = base.clone();
        let dc_toml = project.path.join(".devcontainer/dc.toml");
        if dc_toml.is_file() {
            let contents = std::fs::read_to_string(&dc_toml)?;
            let overrides: ProjectOptions = toml::from_str(&contents)?;
            project.options.apply_overrides(overrides);
        }

        Ok((name, project))
    }
}
