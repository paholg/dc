use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

fn main() -> eyre::Result<()> {
    let root = workspace_root();

    write(
        // This puts the schema at https://devconcurrent.paholg.com/devconcurrent.schema.json for
        // public consumption.
        &root.join("docs/src/devconcurrent.schema.json"),
        &format!("{}\n", schema_json(&root)?),
    )?;

    let generated_path = root.join("docs/src/reference/generated");
    fs::remove_dir_all(&generated_path)?;
    fs::create_dir(&generated_path)?;

    write(&generated_path.join("cli.md"), &cli_markdown())?;
    write(&generated_path.join("config.md"), &config_markdown()?)?;
    write(
        &generated_path.join("customizations.md"),
        &dc_options_markdown()?,
    )?;

    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn write(path: &Path, contents: &str) -> eyre::Result<()> {
    std::fs::write(path, contents)?;
    println!("wrote {}", path.display());
    Ok(())
}

fn schema_json(workspace_root: &Path) -> eyre::Result<String> {
    let cargo_lock = std::fs::read_to_string(workspace_root.join("Cargo.lock"))?;
    let toml_spec_version = toml_spec_version(&cargo_lock)?;

    let mut schema = devconcurrent::schema();
    schema.ensure_object().insert(
        "x-tombi-toml-version".into(),
        format!("v{toml_spec_version}").into(),
    );

    Ok(serde_json::to_string_pretty(&schema)?)
}

fn cli_markdown() -> String {
    let options = clap_markdown::MarkdownOptions::new()
        .title("CLI".to_string())
        .show_footer(false);
    let full = clap_markdown::help_markdown_command_custom(&devconcurrent::cli_command(), &options);

    full
}

fn config_markdown() -> eyre::Result<String> {
    let root = serde_json::to_value(devconcurrent::schema())?;

    let mut out = String::new();
    walk_properties(&mut out, &root, &root["$defs"], 0);

    Ok(out)
}

/// The `customizations.devconcurrent` options, which live in the same schema
/// as config.toml (as `projects.<name>.devcontainer.customizations`).
fn dc_options_markdown() -> eyre::Result<String> {
    let root = serde_json::to_value(devconcurrent::schema())?;
    let node = &root["$defs"]["DcOptions"];
    assert!(!node.is_null(), "no DcOptions in schema $defs");

    let mut out = String::new();
    walk_properties(&mut out, node, &root["$defs"], 0);

    Ok(out)
}

/// Extract the TOML spec version from the `toml` crate's build metadata in
/// Cargo.lock. The crate is versioned as e.g. `1.1.2+spec-1.1.0`, where the
/// part after `+spec-` is the TOML specification version it conforms to.
fn toml_spec_version(cargo_lock: &str) -> eyre::Result<String> {
    let mut lines = cargo_lock.lines();
    while let Some(line) = lines.next() {
        if line != r#"name = "toml""# {
            continue;
        }
        let Some(version_line) = lines.next() else {
            continue;
        };
        let version = version_line
            .strip_prefix(r#"version = ""#)
            .and_then(|s| s.strip_suffix('"'));
        let Some(version) = version else { continue };
        if let Some((_, spec)) = version.split_once("+spec-") {
            return Ok(spec.to_string());
        }
    }
    eyre::bail!("no `toml` package with `+spec-` build metadata in Cargo.lock")
}

/// Emit one bullet per property of `node`, recursing into nested tables.
fn walk_properties(out: &mut String, node: &Value, defs: &Value, depth: usize) {
    let Some(properties) = node.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required: Vec<&str> = node
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    for (name, prop) in properties {
        let (resolved, ref_name) = resolve(prop, defs);

        // A map with arbitrary keys, like `projects` or `services`: document
        // it as `name.<name>` and descend into the value's schema.
        let pattern_value = resolved
            .get("patternProperties")
            .and_then(Value::as_object)
            .and_then(|p| p.values().next())
            .or_else(|| {
                resolved
                    .get("additionalProperties")
                    .filter(|v| v.is_object())
            });
        let (display, target, ref_name) = match pattern_value {
            Some(value) => {
                let (target, ref_name) = resolve(value, defs);
                (format!("{name}.<name>"), target, ref_name)
            }
            None => (name.clone(), resolved, ref_name),
        };

        let description = prop
            .get("description")
            .or_else(|| target.get("description"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        let mut meta = vec![type_of(target)];
        if required.contains(&name.as_str()) {
            meta.push("required".to_string());
        }
        let default = match prop.get("default").or_else(|| target.get("default")) {
            Some(d) if !d.is_null() && !d.is_object() => format!(" [default: `{d}`]"),
            _ => String::new(),
        };

        let indent = "  ".repeat(depth);
        let meta = meta.join(", ");
        // The first paragraph goes on the bullet line; further paragraphs
        // (which may hold code fences) follow verbatim, indented to stay part
        // of the list item.
        let mut paragraphs = description.split("\n\n");
        let summary = paragraphs
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            "{indent}* `{display}` ({meta}){default} — {summary}\n"
        ));
        for paragraph in paragraphs {
            out.push('\n');
            for line in paragraph.lines() {
                out.push_str(&format!("{indent}  {line}\n"));
            }
        }

        let examples = prop
            .get("examples")
            .or_else(|| target.get("examples"))
            .and_then(Value::as_array);
        for example in examples.into_iter().flatten() {
            if example.is_object() || example.is_array() {
                let json = serde_json::to_string_pretty(example).unwrap();
                out.push_str(&format!("\n{indent}  Example:\n\n{indent}  ```json\n"));
                for line in json.lines() {
                    out.push_str(&format!("{indent}  {line}\n"));
                }
                out.push_str(&format!("{indent}  ```\n"));
            } else {
                out.push_str(&format!("\n{indent}  Example: `{example}`\n"));
            }
        }

        // The devcontainer schema is documented by its own spec; don't inline
        // its hundreds of options here.
        if ref_name != Some("DevcontainerConfig") {
            walk_properties(out, target, defs, depth + 1);
        }
    }
}

/// Follow `$ref`s and `anyOf [X, null]` wrappers to the real schema, returning
/// it along with the `$defs` name it came from, if any.
fn resolve<'a>(node: &'a Value, defs: &'a Value) -> (&'a Value, Option<&'a str>) {
    if let Some(name) = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/$defs/"))
        && let Some(def) = defs.get(name)
    {
        return (def, Some(name));
    }
    if let Some(any) = node.get("anyOf").and_then(Value::as_array) {
        let mut non_null = any
            .iter()
            .filter(|v| v.get("type").and_then(Value::as_str) != Some("null"));
        if let (Some(only), None) = (non_null.next(), non_null.next()) {
            return resolve(only, defs);
        }
    }
    (node, None)
}

fn type_of(node: &Value) -> String {
    if node.get("properties").is_some() || node.get("patternProperties").is_some() {
        return "table".to_string();
    }
    match node.get("type") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| *s != "null")
            .collect::<Vec<_>>()
            .join(" | "),
        _ => "value".to_string(),
    }
}
