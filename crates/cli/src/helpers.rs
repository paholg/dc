use std::io::Write;
use std::path::PathBuf;

use serde::Deserialize;

pub(crate) const SHELL_FD: &str = "DEVCONCURRENT_SHELL_FD";
/// Space-separated names of the variables `show env --export` set last time.
///
/// It exports this alongside the variables themselves, then reads it back on
/// the next run to know what to unset — so leaving a workspace, or dropping a
/// variable from the config, doesn't leave a stale value behind.
pub(crate) const EXPORTED_ENV: &str = "DEVCONCURRENT_ENV";

/// Send a shell command to the calling shell (via the `dc` wrapper function).
///
/// If `DEVCONCURRENT_SHELL_FD` names an open file descriptor, write the command
/// there and the wrapper will `eval` it. Otherwise print to stdout so the user
/// can copy it (or pipe to `eval` themselves).
pub(crate) fn forward_to_shell(command: &str) -> eyre::Result<()> {
    if let Ok(fd) = std::env::var(SHELL_FD) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(format!("/dev/fd/{fd}"))?;
        writeln!(f, "{command}")?;
    } else {
        println!("{command}");
    }
    Ok(())
}

/// Simple validator for workspace and project names.
///
/// We use the same rules for both for simplicity.
pub(crate) fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("must not be empty".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!("{name:?} must contain only [a-zA-Z0-9-_]"));
    }
    Ok(())
}

pub(crate) fn deserialize_shell_path_opt<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<PathBuf>, D::Error> {
    Option::<String>::deserialize(d)
        .map(|o| o.map(|s| PathBuf::from(shellexpand::tilde(&s).as_ref())))
}

pub(crate) fn deserialize_shell_path<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<PathBuf, D::Error> {
    let s = String::deserialize(d)?;
    Ok(PathBuf::from(shellexpand::tilde(&s).as_ref()))
}

/// Schemars generates a map with patterns as `patternProperties` with `additionalProperties: false`,
/// which doesn't give an ideal editor experience.
///
/// This allows one to instead emit `propertyNames`.
pub(crate) struct PatternMap<K, V>(std::marker::PhantomData<(K, V)>);

impl<K: schemars::JsonSchema, V: schemars::JsonSchema> schemars::JsonSchema for PatternMap<K, V> {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        format!("Map_from_{}_to_{}", K::schema_name(), V::schema_name()).into()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        format!("Map<{}, {}>", K::schema_id(), V::schema_id()).into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "propertyNames": K::json_schema(generator),
            "additionalProperties": generator.subschema_for::<V>(),
        })
    }
}
