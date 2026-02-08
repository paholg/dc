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

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub default_cmd: Option<Cmd>,

    #[serde(default)]
    pub projects: IndexMap<String, Project>,
}

#[serde_inline_default]
#[derive(Debug, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub default_cmd: Option<Cmd>,
    #[serde(deserialize_with = "deserialize_shell_path")]
    pub path: PathBuf,
    /// Directory to create workspaces in (default /tmp/).
    #[serde_inline_default("/tmp/".into())]
    #[serde(deserialize_with = "deserialize_shell_path")]
    pub workspace_dir: PathBuf,

    /// If set, this port will be used automatically by the `dc fwd` command, to
    /// map a static host port to the container of your choice.
    pub fwd_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectOverride {
    #[serde(default)]
    pub default_cmd: Option<Cmd>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub workspace_dir: Option<PathBuf>,
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

    pub fn project<'a>(&'a self, name: Option<&'a str>) -> eyre::Result<(&'a str, &'a Project)> {
        match name {
            Some(name) => self
                .projects
                .get(name)
                .map(|p| (name, p))
                .ok_or_else(|| eyre!("no project configured with name: {name}")),
            None => self
                .projects
                .iter()
                .next()
                .map(|(n, p)| (n.as_ref(), p))
                .ok_or_else(|| eyre!("no projects configured")),
        }
    }
}
