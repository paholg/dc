use std::fmt;
use std::path::PathBuf;

use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use shared::{ProxyOptions, Template};

use crate::helpers::deserialize_shell_path_opt;

#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DcOptions {
    /// The directory where devconcurrent will place worktrees.
    #[serde(deserialize_with = "deserialize_shell_path_opt")]
    pub(crate) worktree_folder: Option<PathBuf>,
    /// Whether to mount the project's git directory into each workspace's devcontainer.
    ///
    /// Git worktrees have a simple `.git` file that points to the actual `.git` directory. If that
    /// directory isn't available, then no git commands will work. By mounting it at its original
    /// path in the devcontainer, `git` should just work, both inside and out of the container.
    // NOTE: This is an Option to support merging configs.
    #[schemars(extend("default" = true))]
    mount_git: Option<bool>,

    /// Configure DNS hostnames and HTTP proxy.
    pub(crate) proxy: ProxyOptions,

    /// Define shell variables
    ///
    /// These are rendered by `dc show env` or automatically set if `shell.exportEnv` is true.
    ///
    /// The values are given by handlebars templates with the following:
    ///   * The `hostname` helper gives the hostname for a service.
    ///   * The following variables are populated: `project`, `workspace`, and `root`.
    #[schemars(example = serde_json::json!({
        "BASE_URL": "{{ hostname 'app' }}",
        "DATABASE_URL": "postgres://postgres:postgres@{{hostname 'postgres'}}:5432/db"
    }))]
    #[schemars(with = "crate::helpers::PatternMap<EnvVarName, Template>")]
    pub(crate) env: IndexMap<EnvVarName, Template>,
}

impl DcOptions {
    pub(crate) fn mount_git(&self) -> bool {
        self.mount_git.unwrap_or(true)
    }
}

/// A shell variable name.
///
/// `dc show env --export` produces assignments that the calling shell `eval`s,
/// so a name that isn't a valid identifier has to fail at config load rather
/// than turn into broken shell code at the prompt.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct EnvVarName(String);

impl EnvVarName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn new(name: String) -> Result<Self, String> {
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');

        if valid {
            Ok(Self(name))
        } else {
            Err(format!(
                "{name:?} is not a valid shell variable name; expected [A-Za-z_][A-Za-z0-9_]*"
            ))
        }
    }
}

impl fmt::Display for EnvVarName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for EnvVarName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl JsonSchema for EnvVarName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EnvVarName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[A-Za-z_][A-Za-z0-9_]*$",
            "description": "A shell variable name.",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_shell_variable_names() {
        for name in ["DATABASE_URL", "_x", "a1"] {
            EnvVarName::new(name.to_string()).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }

    #[test]
    fn rejects_names_a_shell_cannot_assign() {
        for name in ["", "foo-bar", "2FOO", "a b", "PATH;rm"] {
            assert!(
                EnvVarName::new(name.to_string()).is_err(),
                "accepted {name:?}"
            );
        }
    }
}
