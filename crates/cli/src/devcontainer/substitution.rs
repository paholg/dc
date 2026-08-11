//! Parser for the `${...}` variable syntax used in devcontainer.json.
//!
//! Spec: <https://github.com/devcontainers/spec/blob/main/docs/specs/devcontainerjson-reference.md#variables-in-devcontainerjson>
//!
//! Behaviors mirrored from the reference implementation
//! (<https://github.com/devcontainers/cli/blob/main/src/spec-common/variableSubstitution.ts)>:
//!
//! - Single pass, non-recursive: a resolved value is not re-parsed.
//! - Unknown variable names pass through as literal text.
//! - For env-style variables, only the first colon-separated arg is the name and the second
//!   (if present) is the default; further `:`-segments are silently dropped.
//! - For no-arg variables, any provided args are ignored.
//! - Case sensitive; surrounding whitespace inside `${...}` is not tolerated.

use std::fmt;
use std::path::{Path, PathBuf};

use eyre::WrapErr;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use winnow::{
    ModalResult, Parser,
    combinator::{alt, preceded, repeat},
    token::{literal, take_till, take_while},
};

use crate::devcontainer::DevcontainerLabels;

/// Why a container-phase variable isn't resolvable.
const NO_CONTAINER: &str = "no container exists at this point. It is only available in fields \
    applied after the container is created, such as `remoteEnv` and the lifecycle commands that \
    run in the container";

/// Why the container workspace folder isn't resolvable.
const NO_CONTAINER_WORKSPACE_FOLDER: &str = "the container workspace folder is not known at this \
    point. `workspaceFolder` is what defines it, so it cannot refer to itself";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Variable {
    LocalEnv {
        name: String,
        default: Option<String>,
    },
    ContainerEnv {
        name: String,
        default: Option<String>,
    },
    LocalWorkspaceFolder,
    ContainerWorkspaceFolder,
    LocalWorkspaceFolderBasename,
    ContainerWorkspaceFolderBasename,
    DevcontainerId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Segment {
    Literal(String),
    Var(Variable),
}

/// What a [`Template`] can be rendered against.
///
/// Which variables resolve depends on how far along we are: before the compose
/// file is written we know only the local side, and `${containerEnv:…}` is not
/// answerable until a container exists. A variable used outside its phase is an
/// error rather than an empty string, so a misplaced one is reported instead of
/// silently producing the wrong value.
#[derive(Debug, Clone)]
pub(crate) struct Context<'a> {
    local_env: IndexMap<String, String>,
    local_workspace_folder: &'a Path,
    labels: &'a DevcontainerLabels,
    container_workspace_folder: Option<&'a Path>,
    container_env: Option<&'a IndexMap<String, String>>,
}

impl<'a> Context<'a> {
    /// The local phase: everything except `${containerEnv:…}` and the container
    /// workspace folder.
    pub(crate) fn new(local_workspace_folder: &'a Path, labels: &'a DevcontainerLabels) -> Self {
        Self {
            local_env: std::env::vars().collect(),
            local_workspace_folder,
            labels,
            container_workspace_folder: None,
            container_env: None,
        }
    }

    pub(crate) fn with_container_workspace_folder(mut self, folder: &'a Path) -> Self {
        self.container_workspace_folder = Some(folder);
        self
    }

    pub(crate) fn with_container_env(mut self, env: &'a IndexMap<String, String>) -> Self {
        self.container_env = Some(env);
        self
    }

    /// Render `template`, blaming `field` if a variable isn't available here.
    pub(crate) fn render_field(&self, field: &str, template: &Template) -> eyre::Result<String> {
        template
            .render(self)
            .wrap_err_with(|| format!("in `{field}`"))
    }

    pub(crate) fn render_path(&self, field: &str, template: &Template) -> eyre::Result<PathBuf> {
        self.render_field(field, template).map(PathBuf::from)
    }

    #[cfg(test)]
    fn with_local_env(mut self, local_env: IndexMap<String, String>) -> Self {
        self.local_env = local_env;
        self
    }
}

/// A variable used where it cannot be resolved.
#[derive(Debug)]
pub(crate) struct RenderError {
    variable: Variable,
    reason: &'static str,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} is not available: {}", self.variable, self.reason)
    }
}

impl std::error::Error for RenderError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Template(pub(crate) Vec<Segment>);

impl fmt::Display for Variable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Variable::LocalEnv {
                name,
                default: None,
            } => write!(f, "${{localEnv:{name}}}"),
            Variable::LocalEnv {
                name,
                default: Some(d),
            } => write!(f, "${{localEnv:{name}:{d}}}"),
            Variable::ContainerEnv {
                name,
                default: None,
            } => write!(f, "${{containerEnv:{name}}}"),
            Variable::ContainerEnv {
                name,
                default: Some(d),
            } => write!(f, "${{containerEnv:{name}:{d}}}"),
            Variable::LocalWorkspaceFolder => f.write_str("${localWorkspaceFolder}"),
            Variable::ContainerWorkspaceFolder => f.write_str("${containerWorkspaceFolder}"),
            Variable::LocalWorkspaceFolderBasename => {
                f.write_str("${localWorkspaceFolderBasename}")
            }
            Variable::ContainerWorkspaceFolderBasename => {
                f.write_str("${containerWorkspaceFolderBasename}")
            }
            Variable::DevcontainerId => f.write_str("${devcontainerId}"),
        }
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for seg in &self.0 {
            match seg {
                Segment::Literal(s) => f.write_str(s)?,
                Segment::Var(v) => write!(f, "{v}")?,
            }
        }
        Ok(())
    }
}

impl Serialize for Template {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl schemars::JsonSchema for Template {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Template".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A string that may contain `${...}` variable substitutions. \
                            Supported variables: `${localEnv:VAR[:default]}`, \
                            `${containerEnv:VAR[:default]}`, `${localWorkspaceFolder}`, \
                            `${containerWorkspaceFolder}`, `${localWorkspaceFolderBasename}`, \
                            `${containerWorkspaceFolderBasename}`, `${devcontainerId}`. \
                            See https://containers.dev/implementors/json_reference/#variables-in-devcontainerjson.",
        })
    }
}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        Ok(Template::parse(&s))
    }
}

impl Template {
    pub(crate) fn parse(input: &str) -> Self {
        template
            .parse(input)
            .expect("template parser should be infallible")
    }

    pub(crate) fn render(&self, context: &Context<'_>) -> Result<String, RenderError> {
        let mut out = String::new();
        for segment in &self.0 {
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Var(variable) => out.push_str(&variable.evaluate(context)?),
            }
        }
        Ok(out)
    }
}

impl Variable {
    fn evaluate(&self, context: &Context<'_>) -> Result<String, RenderError> {
        let basename = |path: &Path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let container_workspace_folder = || {
            context
                .container_workspace_folder
                .ok_or_else(|| self.unavailable(NO_CONTAINER_WORKSPACE_FOLDER))
        };
        match self {
            Variable::LocalEnv { name, default } => {
                Ok(env_lookup(&context.local_env, name, default.as_deref()))
            }
            Variable::ContainerEnv { name, default } => match context.container_env {
                Some(env) => Ok(env_lookup(env, name, default.as_deref())),
                None => Err(self.unavailable(NO_CONTAINER)),
            },
            Variable::LocalWorkspaceFolder => Ok(context
                .local_workspace_folder
                .to_string_lossy()
                .into_owned()),
            Variable::ContainerWorkspaceFolder => {
                Ok(container_workspace_folder()?.to_string_lossy().into_owned())
            }
            Variable::LocalWorkspaceFolderBasename => Ok(basename(context.local_workspace_folder)),
            Variable::ContainerWorkspaceFolderBasename => {
                Ok(basename(container_workspace_folder()?))
            }
            Variable::DevcontainerId => Ok(context.labels.devcontainer_id()),
        }
    }

    fn unavailable(&self, reason: &'static str) -> RenderError {
        RenderError {
            variable: self.clone(),
            reason,
        }
    }
}

fn env_lookup(env: &IndexMap<String, String>, name: &str, default: Option<&str>) -> String {
    env.get(name)
        .cloned()
        .or_else(|| default.map(str::to_string))
        .unwrap_or_default()
}

fn template(input: &mut &str) -> ModalResult<Template> {
    let segments: Vec<Segment> = repeat(0.., segment).parse_next(input)?;
    Ok(Template(coalesce_literals(segments)))
}

fn segment(input: &mut &str) -> ModalResult<Segment> {
    alt((
        variable.map(Segment::Var),
        literal_chunk.map(Segment::Literal),
    ))
    .parse_next(input)
}

/// Unknown variable names fail this branch so [`literal_chunk`] absorbs them as text.
fn variable(input: &mut &str) -> ModalResult<Variable> {
    let _ = literal("${").parse_next(input)?;
    let name = take_while(0.., |c: char| c.is_ascii_alphabetic()).parse_next(input)?;
    let args: Vec<&str> = repeat(
        0..,
        preceded(literal(":"), take_till(0.., |c: char| c == ':' || c == '}')),
    )
    .parse_next(input)?;
    let _ = literal("}").parse_next(input)?;

    resolve_name(name, &args)
        .ok_or_else(|| winnow::error::ErrMode::Backtrack(winnow::error::ContextError::new()))
}

fn resolve_name(name: &str, args: &[&str]) -> Option<Variable> {
    match name {
        "localEnv" if !args.is_empty() => Some(Variable::LocalEnv {
            name: args[0].to_string(),
            default: args.get(1).map(std::string::ToString::to_string),
        }),
        "containerEnv" if !args.is_empty() => Some(Variable::ContainerEnv {
            name: args[0].to_string(),
            default: args.get(1).map(std::string::ToString::to_string),
        }),
        "localWorkspaceFolder" => Some(Variable::LocalWorkspaceFolder),
        "containerWorkspaceFolder" => Some(Variable::ContainerWorkspaceFolder),
        "localWorkspaceFolderBasename" => Some(Variable::LocalWorkspaceFolderBasename),
        "containerWorkspaceFolderBasename" => Some(Variable::ContainerWorkspaceFolderBasename),
        "devcontainerId" => Some(Variable::DevcontainerId),
        _ => None,
    }
}

/// Returns Err on empty so `alt` in [`segment`] backtracks to [`variable`].
fn literal_chunk(input: &mut &str) -> ModalResult<String> {
    let mut out = String::new();
    loop {
        if input.is_empty() {
            break;
        }
        if input.starts_with("${") {
            // If this `${...}` parses as a known variable, leave it for the variable branch.
            // Otherwise absorb the whole `${...}` (up to the next `}` or EOF) as literal text.
            let mut probe = *input;
            if variable.parse_next(&mut probe).is_ok() {
                break;
            }
            let bytes = input.as_bytes();
            let mut end = 2.min(bytes.len());
            while end < bytes.len() && bytes[end] != b'}' {
                end += 1;
            }
            if end < bytes.len() {
                end += 1;
            }
            out.push_str(&input[..end]);
            *input = &input[end..];
            continue;
        }
        let next = input.chars().next().unwrap();
        out.push(next);
        *input = &input[next.len_utf8()..];
    }
    if out.is_empty() {
        Err(winnow::error::ErrMode::Backtrack(
            winnow::error::ContextError::new(),
        ))
    } else {
        Ok(out)
    }
}

/// Merges adjacent `Literal` segments produced by back-to-back unknown-`${...}` runs.
fn coalesce_literals(segments: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segments.len());
    for seg in segments {
        match (out.last_mut(), seg) {
            (Some(Segment::Literal(prev)), Segment::Literal(next)) => prev.push_str(&next),
            (_, seg) => out.push(seg),
        }
    }
    out
}

#[cfg(test)]
mod tests {

    use super::*;

    fn lit(s: &str) -> Segment {
        Segment::Literal(s.to_string())
    }

    fn var(v: Variable) -> Segment {
        Segment::Var(v)
    }

    fn string_map(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    struct ContextBuilder {
        local_env: IndexMap<String, String>,
        local_workspace_folder: PathBuf,
        container_workspace_folder: Option<PathBuf>,
        container_env: Option<IndexMap<String, String>>,
        labels: DevcontainerLabels,
    }

    impl ContextBuilder {
        fn new() -> Self {
            Self {
                local_env: IndexMap::new(),
                local_workspace_folder: PathBuf::new(),
                container_workspace_folder: None,
                container_env: None,
                labels: DevcontainerLabels::new(PathBuf::new(), None),
            }
        }

        fn local_env(mut self, pairs: &[(&str, &str)]) -> Self {
            self.local_env = string_map(pairs);
            self
        }

        fn local_workspace_folder(mut self, path: &str) -> Self {
            self.local_workspace_folder = path.into();
            self
        }

        fn container_workspace_folder(mut self, path: &str) -> Self {
            self.container_workspace_folder = Some(path.into());
            self
        }

        fn container_env(mut self, env: &[(&str, &str)]) -> Self {
            self.container_env = Some(string_map(env));
            self
        }

        fn labels(mut self, local_folder: &str, config_file: Option<&str>) -> Self {
            self.labels =
                DevcontainerLabels::new(local_folder.into(), config_file.map(PathBuf::from));
            self
        }

        fn build(&self) -> Context<'_> {
            let mut context = Context::new(&self.local_workspace_folder, &self.labels)
                .with_local_env(self.local_env.clone());
            if let Some(folder) = &self.container_workspace_folder {
                context = context.with_container_workspace_folder(folder);
            }
            if let Some(env) = &self.container_env {
                context = context.with_container_env(env);
            }
            context
        }
    }

    fn render_with(input: &str, builder: ContextBuilder) -> String {
        Template::parse(input)
            .render(&builder.build())
            .expect("variable is available in this context")
    }

    fn render_error(input: &str, builder: ContextBuilder) -> String {
        Template::parse(input)
            .render(&builder.build())
            .expect_err("variable is not available in this context")
            .to_string()
    }

    #[test]
    fn empty_string() {
        assert_eq!(Template::parse("").0, vec![]);
    }

    #[test]
    fn pure_literal() {
        assert_eq!(Template::parse("hello world").0, vec![lit("hello world")]);
    }

    #[test]
    fn lone_dollar_is_literal() {
        assert_eq!(Template::parse("price: $5").0, vec![lit("price: $5")]);
    }

    #[test]
    fn local_env_no_default() {
        assert_eq!(
            Template::parse("${localEnv:HOME}").0,
            vec![var(Variable::LocalEnv {
                name: "HOME".to_string(),
                default: None,
            })]
        );
    }

    #[test]
    fn local_env_with_default() {
        assert_eq!(
            Template::parse("${localEnv:HOME:/tmp}").0,
            vec![var(Variable::LocalEnv {
                name: "HOME".to_string(),
                default: Some("/tmp".to_string()),
            })]
        );
    }

    #[test]
    fn extra_colons_dropped() {
        assert_eq!(
            Template::parse("${localEnv:HOME:def:extra}").0,
            vec![var(Variable::LocalEnv {
                name: "HOME".to_string(),
                default: Some("def".to_string()),
            })]
        );
    }

    #[test]
    fn no_arg_variables() {
        assert_eq!(
            Template::parse("${localWorkspaceFolder}").0,
            vec![var(Variable::LocalWorkspaceFolder)]
        );
        assert_eq!(
            Template::parse("${devcontainerId}").0,
            vec![var(Variable::DevcontainerId)]
        );
    }

    #[test]
    fn no_arg_variable_ignores_args() {
        assert_eq!(
            Template::parse("${localWorkspaceFolder:foo}").0,
            vec![var(Variable::LocalWorkspaceFolder)]
        );
    }

    #[test]
    fn cross_platform_home() {
        assert_eq!(
            Template::parse("${localEnv:HOME}${localEnv:USERPROFILE}").0,
            vec![
                var(Variable::LocalEnv {
                    name: "HOME".to_string(),
                    default: None,
                }),
                var(Variable::LocalEnv {
                    name: "USERPROFILE".to_string(),
                    default: None,
                }),
            ]
        );
    }

    #[test]
    fn mixed_template_parse() {
        assert_eq!(
            Template::parse("${localWorkspaceFolder}/.cache/${localEnv:USER}").0,
            vec![
                var(Variable::LocalWorkspaceFolder),
                lit("/.cache/"),
                var(Variable::LocalEnv {
                    name: "USER".to_string(),
                    default: None,
                }),
            ]
        );
    }

    #[test]
    fn unknown_variable_is_literal() {
        assert_eq!(
            Template::parse("${nope:foo} after").0,
            vec![lit("${nope:foo} after")]
        );
    }

    #[test]
    fn whitespace_inside_braces_unrecognized() {
        assert_eq!(
            Template::parse("${ localEnv:HOME }").0,
            vec![lit("${ localEnv:HOME }")]
        );
    }

    #[test]
    fn case_sensitive() {
        assert_eq!(
            Template::parse("${LocalEnv:HOME}").0,
            vec![lit("${LocalEnv:HOME}")]
        );
    }

    #[test]
    fn unterminated_brace_is_literal() {
        assert_eq!(
            Template::parse("${localEnv:HOME").0,
            vec![lit("${localEnv:HOME")]
        );
    }

    #[test]
    fn local_env_without_arg_is_unknown() {
        // Reference impl throws here; we pass through as literal instead.
        assert_eq!(Template::parse("${localEnv}").0, vec![lit("${localEnv}")]);
    }

    #[test]
    fn empty_arg() {
        assert_eq!(
            Template::parse("${localEnv:}").0,
            vec![var(Variable::LocalEnv {
                name: String::new(),
                default: None,
            })]
        );
    }

    #[test]
    fn back_to_back_unknowns() {
        assert_eq!(Template::parse("${a}${b}").0, vec![lit("${a}${b}")]);
    }

    #[test]
    fn unknown_then_known() {
        assert_eq!(
            Template::parse("${a}${localWorkspaceFolder}").0,
            vec![lit("${a}"), var(Variable::LocalWorkspaceFolder)]
        );
    }

    #[test]
    fn render_local_env_present() {
        assert_eq!(
            render_with(
                "${localEnv:HOME}",
                ContextBuilder::new().local_env(&[("HOME", "/home/me")]),
            ),
            "/home/me",
        );
    }

    #[test]
    fn render_local_env_missing_uses_default() {
        assert_eq!(
            render_with("${localEnv:X:fallback}", ContextBuilder::new()),
            "fallback",
        );
    }

    #[test]
    fn render_local_env_missing_no_default_is_empty() {
        assert_eq!(render_with("${localEnv:X}", ContextBuilder::new()), "");
    }

    #[test]
    fn render_container_env() {
        assert_eq!(
            render_with(
                "${containerEnv:PATH}",
                ContextBuilder::new().container_env(&[("PATH", "/usr/bin")]),
            ),
            "/usr/bin",
        );
    }

    #[test]
    fn container_env_without_a_container_is_an_error() {
        let err = render_error("${containerEnv:PATH}", ContextBuilder::new());
        assert!(
            err.contains("${containerEnv:PATH} is not available"),
            "{err}"
        );
        assert!(err.contains("no container exists"), "{err}");
    }

    /// A default doesn't make the variable answerable — the field it was
    /// written in is simply the wrong place for it.
    #[test]
    fn container_env_default_does_not_mask_an_unavailable_container() {
        let err = render_error("${containerEnv:PATH:/bin}", ContextBuilder::new());
        assert!(err.contains("${containerEnv:PATH:/bin}"), "{err}");
    }

    #[test]
    fn container_workspace_folder_is_an_error_where_it_is_undefined() {
        for input in [
            "${containerWorkspaceFolder}",
            "${containerWorkspaceFolderBasename}",
        ] {
            let err = render_error(input, ContextBuilder::new());
            assert!(err.contains("cannot refer to itself"), "{err}");
        }
    }

    #[test]
    fn render_workspace_folders() {
        let b = ContextBuilder::new()
            .local_workspace_folder("/host/projects/myrepo")
            .container_workspace_folder("/workspaces/myrepo");
        assert_eq!(
            render_with("${localWorkspaceFolder}", b.clone_for_test()),
            "/host/projects/myrepo",
        );
        assert_eq!(
            render_with("${localWorkspaceFolderBasename}", b.clone_for_test()),
            "myrepo",
        );
        assert_eq!(
            render_with("${containerWorkspaceFolder}", b.clone_for_test()),
            "/workspaces/myrepo",
        );
        assert_eq!(
            render_with("${containerWorkspaceFolderBasename}", b),
            "myrepo",
        );
    }

    /// The id hashes labels we write ourselves, so it resolves with no
    /// container in sight, and agrees with hashing the labels a container
    /// would report.
    #[test]
    fn render_devcontainer_id_without_a_container() {
        let expected = crate::docker::probe::devcontainer_id(
            [
                (docker::LOCAL_FOLDER_LABEL, "/foo"),
                (docker::CONFIG_FILE_LABEL, "/foo/.devcontainer.json"),
            ]
            .into_iter(),
        );
        assert_eq!(
            render_with(
                "${devcontainerId}",
                ContextBuilder::new().labels("/foo", Some("/foo/.devcontainer.json")),
            ),
            expected,
        );
    }

    #[test]
    fn render_extra_colons_dropped_in_default() {
        assert_eq!(
            render_with("${localEnv:X:def:extra}", ContextBuilder::new()),
            "def",
        );
    }

    #[test]
    fn render_mixed_template() {
        assert_eq!(
            render_with(
                "${localWorkspaceFolder}/.cache/${localEnv:USER}",
                ContextBuilder::new()
                    .local_env(&[("USER", "paho")])
                    .local_workspace_folder("/work/myrepo"),
            ),
            "/work/myrepo/.cache/paho",
        );
    }

    #[test]
    fn render_unknown_variable_passes_through() {
        assert_eq!(
            render_with("hello ${nope:foo}!", ContextBuilder::new()),
            "hello ${nope:foo}!",
        );
    }

    #[test]
    fn render_cross_platform_home() {
        // USERPROFILE unset → "" → HOME wins.
        assert_eq!(
            render_with(
                "${localEnv:HOME}${localEnv:USERPROFILE}",
                ContextBuilder::new().local_env(&[("HOME", "/home/me")]),
            ),
            "/home/me",
        );
    }

    impl ContextBuilder {
        fn clone_for_test(&self) -> ContextBuilder {
            ContextBuilder {
                local_env: self.local_env.clone(),
                local_workspace_folder: self.local_workspace_folder.clone(),
                container_workspace_folder: self.container_workspace_folder.clone(),
                container_env: self.container_env.clone(),
                labels: self.labels.clone(),
            }
        }
    }
}
