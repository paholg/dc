use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, serde_as};

pub mod lifecycle_command;
pub mod spec;

#[serde_as]
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct DevContainer {
    #[serde_as(as = "OneOrMany<_>")]
    pub docker_compose_file: Vec<String>,
    pub service: String,
    pub run_services: Vec<String>,
    pub workspace_folder: String,
    pub override_command: Option<bool>,
    pub shutdown_action: Option<spec::ComposeShutdownAction>,
}
