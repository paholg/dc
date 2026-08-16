use std::borrow::Cow;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use vec1::{Vec1, vec1};

use crate::devcontainer::substitution::{Context, Template};
use crate::run;

/// A command as written in config, before `${...}` substitution.
#[derive(Debug, Deserialize, Serialize, Clone, JsonSchema)]
#[serde(untagged)]
pub(crate) enum CmdTemplate {
    Shell(Template),
    #[schemars(with = "Vec<Template>")]
    Args(Vec1<Template>),
}

impl CmdTemplate {
    /// `field` names the config property, so an unavailable variable can say
    /// where it was written.
    pub(crate) fn render(&self, field: &str, context: &Context<'_>) -> eyre::Result<Cmd> {
        match self {
            CmdTemplate::Shell(prog) => Ok(Cmd::Shell(context.render_field(field, prog)?)),
            CmdTemplate::Args(args) => {
                let args: Vec<String> = args
                    .iter()
                    .map(|arg| context.render_field(field, arg))
                    .collect::<eyre::Result<_>>()?;
                Ok(Cmd::Args(
                    Vec1::try_from_vec(args).expect("a Vec1 renders to at least one argument"),
                ))
            }
        }
    }
}

/// A command ready to run.
#[derive(Debug, Clone)]
pub(crate) enum Cmd {
    Shell(String),
    Args(Vec1<String>),
}

impl Cmd {
    pub(crate) fn as_args(&self) -> Vec<&str> {
        match self {
            Cmd::Shell(prog) => vec!["/bin/sh", "-c", prog],
            Cmd::Args(args) => args.iter().map(std::string::String::as_str).collect(),
        }
    }

    pub(crate) fn description(&self) -> Cow<'_, str> {
        match &self {
            Cmd::Shell(prog) => prog.into(),
            Cmd::Args(vec1) => vec1.join(" ").into(),
        }
    }
}

impl From<std::process::Command> for Cmd {
    fn from(cmd: std::process::Command) -> Self {
        let mut args = vec1![cmd.get_program().to_string_lossy().to_string()];
        args.extend(cmd.get_args().map(|a| a.to_string_lossy().to_string()));

        Self::Args(args)
    }
}

pub(crate) struct NamedCmd<'a> {
    pub(crate) name: &'a str,
    pub(crate) cmd: &'a Cmd,
    pub(crate) dir: Option<&'a Path>,
}

impl run::Runnable for NamedCmd<'_> {
    fn name(&self) -> Cow<'_, str> {
        self.name.into()
    }

    fn description(&self) -> Cow<'_, str> {
        self.cmd.description()
    }

    async fn run(self, _: run::Token) -> eyre::Result<()> {
        let argv = self.cmd.as_args();
        let what = match self.dir {
            Some(dir) => format!("`{}` in {}", self.cmd.description(), dir.display()),
            None => format!("`{}`", self.cmd.description()),
        };
        super::run_cmd(&argv, self.dir, &what).await
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::LazyLock;

    use super::*;
    use crate::devcontainer::DevcontainerLabels;

    static LABELS: LazyLock<DevcontainerLabels> =
        LazyLock::new(|| DevcontainerLabels::new(PathBuf::from("/host/myrepo"), None));

    fn ctx() -> Context<'static> {
        Context::new(Path::new("/host/myrepo"), &LABELS)
            .with_container_workspace_folder(Path::new("/workspaces/myrepo"))
    }

    fn render(json: &str) -> Cmd {
        serde_json::from_str::<CmdTemplate>(json)
            .expect("valid command")
            .render("postCreateCommand", &ctx())
            .expect("variables are available")
    }

    #[test]
    fn shell_form_substitutes() {
        let cmd = render(r#""ls ${containerWorkspaceFolder}""#);
        assert_eq!(cmd.as_args(), ["/bin/sh", "-c", "ls /workspaces/myrepo"]);
    }

    #[test]
    fn each_argument_substitutes_separately() {
        let cmd =
            render(r#"["ls", "${containerWorkspaceFolder}", "${localWorkspaceFolderBasename}"]"#);
        assert_eq!(cmd.as_args(), ["ls", "/workspaces/myrepo", "myrepo"]);
    }

    /// Shell syntax isn't ours to interpret; only the known variable names are.
    #[test]
    fn shell_variables_pass_through() {
        let cmd = render(r#""echo ${HOME} $PATH""#);
        assert_eq!(cmd.as_args(), ["/bin/sh", "-c", "echo ${HOME} $PATH"]);
    }

    #[test]
    fn an_unavailable_variable_names_the_field() {
        let err = serde_json::from_str::<CmdTemplate>(r#""echo ${containerEnv:FOO}""#)
            .expect("valid command")
            .render("initializeCommand", &ctx())
            .expect_err("no container env in this context");
        let err = format!("{err:#}");
        assert!(err.contains("initializeCommand"), "{err}");
        assert!(err.contains("${containerEnv:FOO}"), "{err}");
    }
}
