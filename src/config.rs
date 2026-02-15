use std::path::PathBuf;

use eyre::{WrapErr, eyre};
use indexmap::IndexMap;
use serde::Deserialize;

pub fn deserialize_shell_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<PathBuf, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PathBuf::from(shellexpand::tilde(&s).as_ref()))
}

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub projects: IndexMap<String, Project>,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    #[serde(deserialize_with = "deserialize_shell_path")]
    pub path: PathBuf,
}

impl Config {
    pub fn load() -> eyre::Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "dc")
            .ok_or_else(|| eyre::eyre!("could not determine config directory"))?;
        let path = dirs.config_dir().join("config.toml");
        let cfg = config::Config::builder()
            .add_source(config::File::from(path.clone()))
            .build()
            .wrap_err_with(|| format!("failed to load {}", path.display()))?;
        serde_path_to_error::deserialize(cfg)
            .wrap_err_with(|| format!("failed to parse {}", path.display()))
    }

    pub fn project(mut self, project_name: Option<String>) -> eyre::Result<(String, Project)> {
        let name = project_name.or_else(|| std::env::var("DC_PROJECT").ok());
        match name {
            Some(name) => self
                .projects
                .swap_remove_entry(&name)
                .ok_or_else(|| eyre!("no project configured with name: {name:?}")),
            None => self
                .projects
                .into_iter()
                .next()
                .ok_or_else(|| eyre!("no projects configured")),
        }
    }
}
