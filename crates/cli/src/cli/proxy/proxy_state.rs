use std::collections::BTreeMap;
use std::path::Path;

use color_eyre::owo_colors::OwoColorize;
use docker::Docker;
use eyre::{Result, WrapErr};
use sha2::{Digest, Sha256};
use shared::{PROXY_CONFIG_DIR, PROXY_CONFIG_FILE, PROXY_CONTAINER_NAME, ProxyOptions};

use super::PROXY_IMAGE;
use crate::config::{Config, Project, ProxyGlobal};
use crate::devcontainer::DevcontainerConfig;
use crate::state::State;
use crate::workspace::Workspace;

// The proxy is different than most commands; it needs to know about potentially all projects, but
// it can also accept a workspace to override the project configuration, so that proxy settings can
// be edited and tested in a workspace.
pub(crate) struct ProxyState {
    pub(crate) docker: Docker,
    pub(crate) config: ProxyGlobal,
    pub(crate) options: BTreeMap<String, ProxyOptions>,
}

impl ProxyState {
    pub(crate) async fn resolve(
        project: Option<String>,
        workspace: Option<String>,
    ) -> Result<Self> {
        let config = Config::load()?;
        let state = State::new(project, &config).await?;
        let workspace = state.resolve_workspace(workspace).await.ok();
        Self::from_workspace(&config, workspace.as_ref()).await
    }

    pub(crate) async fn from_workspace(
        config: &Config,
        workspace: Option<&Workspace<'_>>,
    ) -> Result<Self> {
        // Reuse the docker connection the workspace already opened, if any.
        let docker = match workspace.and_then(|w| w.state.devcontainer.as_ref()) {
            Some(dc) => dc.docker().await?.client.clone(),
            None => Docker::connect().await.wrap_err("connect to docker")?,
        };

        let mut options = BTreeMap::new();
        for (name, project) in &config.projects {
            // Apply workspace override if relevant.
            let workspace_dir = if let Some(ws) = workspace
                && &ws.state.project_name == name
            {
                ws.path.as_path()
            } else {
                project.path.as_path()
            };

            if let Some(opts) = load_proxy_options(project, workspace_dir)? {
                options.insert(name.to_string(), opts);
            }
        }

        Ok(Self {
            docker,
            config: config.proxy.clone(),
            options,
        })
    }

    /// Hash of all the inputs for the proxy, stored in a label, so we can tell if the proxy is
    /// "fresh".
    pub(crate) fn config_hash(&self) -> String {
        config_hash(&self.config, &self.options)
    }

    /// Push all proxy options to the proxy config volume so it can access them.
    pub(crate) async fn push_configs(&self) -> Result<()> {
        let bytes =
            serde_json::to_vec_pretty(&self.options).wrap_err("serialize proxy projects")?;

        let tar = docker::build_single_file_tar(PROXY_CONFIG_FILE, &bytes);
        self.docker
            .upload_archive(PROXY_CONTAINER_NAME, PROXY_CONFIG_DIR, tar)
            .await
            .wrap_err("upload proxy projects")?;

        tracing::info!(
            "{} pushed config for {} project(s): {}",
            "✓".green(),
            self.options.len(),
            self.options.keys().cloned().collect::<Vec<_>>().join(", ")
        );

        Ok(())
    }
}

fn load_proxy_options(project: &Project, workspace_dir: &Path) -> Result<Option<ProxyOptions>> {
    let dc_path = DevcontainerConfig::find_config(workspace_dir);
    let Some(dc_config) = DevcontainerConfig::load(dc_path.as_deref(), project)? else {
        return Ok(None);
    };

    let proxy_options = &dc_config.customizations.devconcurrent.proxy;
    if !proxy_options.enable {
        return Ok(None);
    }

    Ok(Some(proxy_options.clone()))
}

fn config_hash(proxy: &ProxyGlobal, options: &BTreeMap<String, ProxyOptions>) -> String {
    let input = serde_json::json!({
        "image": *PROXY_IMAGE,
        "proxy": proxy,
        "projects": options,
    });

    let json = serde_json::to_string(&input).expect("json value always serializes");
    let digest = Sha256::digest(json.as_bytes());

    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn opts(value: serde_json::Value) -> ProxyOptions {
        serde_json::from_value(value).expect("valid proxy options")
    }

    fn proxy(port: u16, ca_root: Option<&str>) -> ProxyGlobal {
        ProxyGlobal {
            port,
            ca_root: ca_root.map(PathBuf::from),
        }
    }

    fn options(entries: &[(&str, &serde_json::Value)]) -> BTreeMap<String, ProxyOptions> {
        entries
            .iter()
            .map(|(name, value)| (name.to_string(), opts((*value).clone())))
            .collect()
    }

    #[test]
    fn hash_is_deterministic_and_a_valid_label_value() {
        let proxy = proxy(43770, Some("/home/user/.local/share/mkcert"));
        let options = options(&[(
            "proj",
            &json!({
                "enable": true,
                "services": {"web": {"ports": [{"host": 8080, "container": 80}]}},
            }),
        )]);
        let a = config_hash(&proxy, &options);
        let b = config_hash(&proxy, &options);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(
            a.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "unexpected character in {a}",
        );
    }

    #[test]
    fn hash_changes_when_any_input_changes() {
        let enabled = json!({"enable": true});
        let base = config_hash(&proxy(43770, Some("/ca")), &options(&[("proj", &enabled)]));

        let port_changed = config_hash(&proxy(43771, Some("/ca")), &options(&[("proj", &enabled)]));
        assert_ne!(base, port_changed);

        let ca_root_unset = config_hash(&proxy(43770, None), &options(&[("proj", &enabled)]));
        assert_ne!(base, ca_root_unset);

        let options_changed = config_hash(
            &proxy(43770, Some("/ca")),
            &options(&[(
                "proj",
                &json!({
                    "enable": true,
                    "services": {"web": {"ports": [{"host": 8080, "container": 80}]}},
                }),
            )]),
        );
        assert_ne!(base, options_changed);

        let project_added = config_hash(
            &proxy(43770, Some("/ca")),
            &options(&[("proj", &enabled), ("other", &enabled)]),
        );
        assert_ne!(base, project_added);
    }

    #[test]
    fn hash_independent_of_project_insertion_order() {
        let a = json!({"enable": true});
        let b = json!({
            "enable": true,
            "services": {"api": {"ports": [{"host": 3001, "container": 3000}]}},
        });
        let forward = config_hash(&proxy(43770, None), &options(&[("a", &a), ("b", &b)]));
        let reverse = config_hash(&proxy(43770, None), &options(&[("b", &b), ("a", &a)]));
        assert_eq!(forward, reverse);
    }
}
