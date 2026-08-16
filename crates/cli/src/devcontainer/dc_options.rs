use std::fmt;
use std::path::PathBuf;

use indexmap::IndexMap;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};

use shared::{ProxyOptions, Template};

use crate::helpers::deserialize_shell_path_opt;
use crate::run::cmd::CmdTemplate;

#[derive(Deserialize, Serialize, Debug, Clone, Default, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct DcOptions {
    pub(crate) default_exec: Option<CmdTemplate>,
    #[serde(deserialize_with = "deserialize_shell_path_opt")]
    pub(crate) worktree_folder: Option<PathBuf>,
    /// Whether to mount the project's git directory into each workspace's devcontainer.
    ///
    /// Git worktrees have a simple `.git` file that points to the actual `.git` directory. If that
    /// directory isn't available, then no git commands will work in the worktree. By mounting it
    /// at its original path in the devcontainer, we allow you to use `git` freely for the workspace,
    /// both inside and out of the devcontainer.
    ///
    /// Defaults to true, but we use Option so it can be overridden.
    mount_git: Option<bool>,
    /// Reverse-proxy configuration.
    ///
    /// Leave empty if you don't wish to use it.
    pub(crate) proxy: ProxyOptions,

    /// Shell variables describing the current workspace, rendered by
    /// `dc show env`.
    ///
    /// Each value is a Handlebars template. `{{hostname 'svc'}}` expands to the
    /// proxied hostname of compose service `svc`; `project`, `workspace` and
    /// `root` are available as plain variables. For example:
    ///
    /// ```json
    /// "env": {
    ///   "DATABASE_URL": "postgres://postgres:postgres@{{hostname 'postgres'}}:5432/db"
    /// }
    /// ```
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
