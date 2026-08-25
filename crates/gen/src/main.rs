use std::{
    collections::BTreeSet,
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

    let snippets = root.join("docs/snippets");

    write(&generated_path.join("cli.md"), &cli_markdown())?;
    write(
        &generated_path.join("config-toml.md"),
        &config_markdown(&snippets.join("config-toml"))?,
    )?;
    write(
        &generated_path.join("customizations.md"),
        &dc_options_markdown(&snippets.join("customizations"))?,
    )?;
    write(
        &generated_path.join("devcontainer-json.md"),
        &devcontainer_markdown(&snippets.join("devcontainer-json"))?,
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
    let object = schema.ensure_object();
    object.shift_insert(
        1,
        "$id".into(),
        "https://devconcurrent.paholg.com/devconcurrent.schema.json".into(),
    );
    object.insert(
        "x-tombi-toml-version".into(),
        format!("v{toml_spec_version}").into(),
    );

    Ok(serde_json::to_string_pretty(&schema)?)
}

fn cli_markdown() -> String {
    let options = clap_markdown::MarkdownOptions::new()
        .title("CLI".to_string())
        .show_footer(false);

    clap_markdown::help_markdown_command_custom(&devconcurrent::cli_command(), &options)
}

fn config_markdown(snippets: &Path) -> eyre::Result<String> {
    let root = serde_json::to_value(devconcurrent::schema())?;
    render(&root, &root["definitions"], Format::Toml, snippets)
}

/// The `customizations.devconcurrent` options, which live in the same schema
/// as config.toml (as `projects.<name>.devcontainer.customizations`).
fn dc_options_markdown(snippets: &Path) -> eyre::Result<String> {
    let root = serde_json::to_value(devconcurrent::schema())?;
    let node = &root["definitions"]["DcOptions"];
    assert!(!node.is_null(), "no DcOptions in schema definitions");

    render(node, &root["definitions"], Format::Json, snippets)
}

/// The `devcontainer.json` options devconcurrent understands.
fn devcontainer_markdown(snippets: &Path) -> eyre::Result<String> {
    let root = serde_json::to_value(devconcurrent::schema())?;
    let node = &root["definitions"]["DevcontainerConfig"];
    assert!(
        !node.is_null(),
        "no DevcontainerConfig in schema definitions"
    );

    render(node, &root["definitions"], Format::Json, snippets)
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

/// The language the documented file is written in, which determines how
/// defaults and examples are rendered.
#[derive(Clone, Copy)]
enum Format {
    Toml,
    Json,
}

/// One documented option, rendered as its own linkable section.
struct Entry {
    /// Full dotted path, e.g. `projects.<name>.path`.
    display: String,
    /// Anchor id, e.g. `projects-path`.
    id: String,
    depth: usize,
    /// Collapsed first sentence of the description, for the index.
    summary: String,
    /// Everything below the heading: description, metadata, example.
    body: String,
}

/// Render a schema's properties as an index followed by one heading-anchored
/// section per option, with markdown snippets from `snippets` merged in.
fn render(node: &Value, defs: &Value, format: Format, snippets: &Path) -> eyre::Result<String> {
    let mut entries = Vec::new();
    let mut used_snippets = BTreeSet::new();
    collect(
        &mut entries,
        node,
        defs,
        "",
        0,
        format,
        snippets,
        &mut used_snippets,
    )?;
    check_unused_snippets(snippets, &used_snippets)?;

    let mut out =
        String::from("<!-- Generated by crates/gen; do not edit. Run `just gen`. -->\n\n");
    for entry in &entries {
        let indent = "  ".repeat(entry.depth);
        let Entry {
            display,
            id,
            summary,
            ..
        } = entry;
        if summary.is_empty() {
            out.push_str(&format!("{indent}- [`{display}`](#{id})\n"));
        } else {
            out.push_str(&format!("{indent}- [`{display}`](#{id}) — {summary}\n"));
        }
    }
    for entry in &entries {
        let level = "#".repeat((entry.depth + 2).min(6));
        let Entry {
            display, id, body, ..
        } = entry;
        out.push_str(&format!("\n{level} `{display}` {{#{id}}}\n\n{body}"));
    }

    Ok(out)
}

/// Every snippet file must document an option that exists, so stale snippets
/// fail generation instead of silently disappearing from the docs.
fn check_unused_snippets(snippets: &Path, used: &BTreeSet<PathBuf>) -> eyre::Result<()> {
    if !snippets.is_dir() {
        return Ok(());
    }
    let mut unused: Vec<PathBuf> = fs::read_dir(snippets)?
        .map(|e| Ok(e?.path()))
        .collect::<eyre::Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| !used.contains(p))
        .collect();
    unused.sort();
    if !unused.is_empty() {
        eyre::bail!(
            "snippets matching no schema option (rename to an option's dotted path):\n{}",
            unused
                .iter()
                .map(|p| format!("  {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

/// Walk `node`'s properties depth-first, producing one [`Entry`] per option.
#[expect(clippy::too_many_arguments)]
fn collect(
    entries: &mut Vec<Entry>,
    node: &Value,
    defs: &Value,
    prefix: &str,
    depth: usize,
    format: Format,
    snippets: &Path,
    used_snippets: &mut BTreeSet<PathBuf>,
) -> eyre::Result<()> {
    let Some(properties) = node.get("properties").and_then(Value::as_object) else {
        return Ok(());
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
        let (display_name, target, ref_name) = match map_value(resolved) {
            Some(value) => {
                let (target, ref_name) = resolve(value, defs);
                (format!("{name}.<name>"), target, ref_name)
            }
            None => (name.clone(), resolved, ref_name),
        };

        let display = if prefix.is_empty() {
            display_name
        } else {
            format!("{prefix}.{display_name}")
        };
        document(
            entries,
            prop,
            target,
            ref_name,
            &display,
            depth,
            required.contains(&name.as_str()),
            defs,
            format,
            snippets,
            used_snippets,
        )?;
    }
    Ok(())
}

/// Document a single option: emit its entry, then descend into any inner
/// formats (struct fields, array element structs, map values).
#[expect(clippy::too_many_arguments)]
fn document(
    entries: &mut Vec<Entry>,
    prop: &Value,
    target: &Value,
    ref_name: Option<&str>,
    display: &str,
    depth: usize,
    required: bool,
    defs: &Value,
    format: Format,
    snippets: &Path,
    used_snippets: &mut BTreeSet<PathBuf>,
) -> eyre::Result<()> {
    let id = display
        .split('.')
        .map(|s| {
            s.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_lowercase()
        })
        .collect::<Vec<_>>()
        .join("-");

    let description = prop
        .get("description")
        .or_else(|| target.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let mut body = String::new();
    if !description.is_empty() {
        body.push_str(description.trim_end());
        body.push_str("\n\n");
    }

    body.push_str(&format!(
        "**Type**: `{}`\n\n",
        type_of(target, defs, format)
    ));
    if let Some(values) = enum_values(target) {
        body.push_str(&values_block(&values, format)?);
    }
    if required {
        body.push_str("**Required**: yes\n\n");
    }
    if let Some(default) = prop.get("default").or_else(|| target.get("default"))
        && !default.is_null()
        && !default.is_object()
    {
        body.push_str(&format!("**Default**: `{}`\n\n", scalar(default, format)?));
    }

    let snippet_path = snippets.join(format!("{display}.md"));
    if snippet_path.is_file() {
        used_snippets.insert(snippet_path.clone());
        let snippet = fs::read_to_string(&snippet_path)?;
        body.push_str(snippet.trim_end());
        body.push_str("\n\n");
    } else if let Some(examples) = prop
        .get("examples")
        .or_else(|| target.get("examples"))
        .and_then(Value::as_array)
    {
        for example in examples {
            body.push_str(&example_block(example, format)?);
        }
    }

    // Trim the trailing blank line; `render` re-separates sections.
    body.truncate(body.trim_end().len());
    body.push('\n');

    let summary = first_sentence(description);
    entries.push(Entry {
        display: display.to_string(),
        id,
        depth,
        summary,
        body,
    });

    // The devcontainer schema is documented by its own spec; don't inline
    // its hundreds of options here.
    if ref_name == Some("DevcontainerConfig") {
        return Ok(());
    }
    collect(
        entries,
        target,
        defs,
        display,
        depth + 1,
        format,
        snippets,
        used_snippets,
    )?;

    // Inner formats hidden behind `array` or `object` in the type line, in
    // the schema itself or any of its union variants: document array element
    // structs as `name[].field` and map values as `name.<name>`.
    let mut variants = Vec::new();
    union_variants(target, defs, &mut variants);
    for variant in variants {
        if is_array(variant)
            && let Some((items, item_ref)) = variant.get("items").map(|i| resolve(i, defs))
            && item_ref != Some("DevcontainerConfig")
            && let Some(element) = object_variant(items, defs)
        {
            collect(
                entries,
                element,
                defs,
                &format!("{display}[]"),
                depth + 1,
                format,
                snippets,
                used_snippets,
            )?;
        }
        // A map-typed option itself is unwrapped to `name.<name>` by
        // `collect`, so a map here is a union variant or a nested map value.
        if let Some(value) = map_value(variant) {
            let (value_target, value_ref) = resolve(value, defs);
            document(
                entries,
                value,
                value_target,
                value_ref,
                &format!("{display}.<name>"),
                depth + 1,
                false,
                defs,
                format,
                snippets,
                used_snippets,
            )?;
        }
    }
    Ok(())
}

/// The value schema of a map with arbitrary keys, if `node` is one.
fn map_value(node: &Value) -> Option<&Value> {
    node.get("patternProperties")
        .and_then(Value::as_object)
        .and_then(|p| p.values().next())
        .or_else(|| node.get("additionalProperties").filter(|v| v.is_object()))
}

/// Flatten a (possibly nested) union into its leaf variant schemas. A
/// non-union schema is its own single leaf.
fn union_variants<'a>(node: &'a Value, defs: &'a Value, out: &mut Vec<&'a Value>) {
    match node
        .get("anyOf")
        .or_else(|| node.get("oneOf"))
        .and_then(Value::as_array)
    {
        Some(variants) => {
            for variant in variants {
                union_variants(resolve(variant, defs).0, defs, out);
            }
        }
        None => out.push(node),
    }
}

/// Whether a schema's `type` is or includes `array`.
fn is_array(node: &Value) -> bool {
    match node.get("type") {
        Some(Value::String(s)) => s == "array",
        Some(Value::Array(a)) => a.iter().any(|v| v.as_str() == Some("array")),
        _ => false,
    }
}

/// The schema itself if it describes a struct, or the first struct variant of
/// a union like `string | Mount`.
fn object_variant<'a>(node: &'a Value, defs: &'a Value) -> Option<&'a Value> {
    if node.get("properties").is_some() {
        return Some(node);
    }
    node.get("anyOf")
        .or_else(|| node.get("oneOf"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|variant| resolve(variant, defs).0)
        .find(|variant| variant.get("properties").is_some())
}

/// The allowed values of an enum-like schema: a plain `enum` list, or the
/// `oneOf`-of-`const`s that schemars emits for enums with documented variants.
fn enum_values(node: &Value) -> Option<Vec<(&Value, Option<&str>)>> {
    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        return Some(values.iter().map(|v| (v, None)).collect());
    }
    let one_of = node.get("oneOf").and_then(Value::as_array)?;
    one_of
        .iter()
        .map(|variant| {
            let value = variant.get("const")?;
            let description = variant.get("description").and_then(Value::as_str);
            Some((value, description))
        })
        .collect()
}

/// Allowed values as an inline list, or a bulleted list when any value has its
/// own description.
fn values_block(values: &[(&Value, Option<&str>)], format: Format) -> eyre::Result<String> {
    let mut out = String::from("**Values**:");
    if values.iter().any(|(_, description)| description.is_some()) {
        out.push('\n');
        for (value, description) in values {
            out.push_str(&format!("\n- `{}`", scalar(value, format)?));
            if let Some(description) = description {
                out.push_str(&format!(" — {}", first_sentence(description)));
            }
        }
    } else {
        let rendered = values
            .iter()
            .map(|(value, _)| Ok(format!("`{}`", scalar(value, format)?)))
            .collect::<eyre::Result<Vec<_>>>()?
            .join(", ");
        out.push_str(&format!(" {rendered}"));
    }
    out.push_str("\n\n");
    Ok(out)
}

/// The first sentence of the description's first paragraph, collapsed to one
/// line for the index.
fn first_sentence(description: &str) -> String {
    let paragraph = description
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match paragraph.split_once(". ") {
        Some((sentence, _)) => format!("{sentence}."),
        None => paragraph,
    }
}

/// A scalar (or array) value as a literal in the target language.
fn scalar(value: &Value, format: Format) -> eyre::Result<String> {
    match format {
        Format::Json => Ok(value.to_string()),
        Format::Toml => {
            // Serialize as a keyed document and strip the key, since bare
            // values aren't valid TOML documents.
            let doc: toml::Table = toml::Table::try_from(serde_json::json!({ "x": value }))?;
            let rendered = toml::to_string(&doc)?;
            Ok(rendered
                .trim_end()
                .strip_prefix("x = ")
                .unwrap_or(rendered.trim_end())
                .to_string())
        }
    }
}

/// A schema example as a fenced block (or inline code for scalars) in the
/// target language.
fn example_block(example: &Value, format: Format) -> eyre::Result<String> {
    if !example.is_object() && !example.is_array() {
        return Ok(format!("Example: `{}`\n\n", scalar(example, format)?));
    }
    let (lang, rendered) = match format {
        Format::Json => ("json", serde_json::to_string_pretty(example)?),
        Format::Toml => ("toml", toml::to_string_pretty(example)?.trim_end().into()),
    };
    Ok(format!("Example:\n\n```{lang}\n{rendered}\n```\n\n"))
}

/// Follow `$ref`s, single-element `allOf` wrappers (how draft-07 attaches
/// keywords beside a `$ref`), and `anyOf [X, null]` wrappers to the real
/// schema, returning it along with the `definitions` name it came from, if
/// any.
fn resolve<'a>(node: &'a Value, defs: &'a Value) -> (&'a Value, Option<&'a str>) {
    if let Some(name) = node
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|r| r.strip_prefix("#/definitions/"))
        && let Some(def) = defs.get(name)
    {
        return (def, Some(name));
    }
    if let Some([only]) = node
        .get("allOf")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
    {
        return resolve(only, defs);
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

fn type_of(node: &Value, defs: &Value, format: Format) -> String {
    let types = type_parts(node, defs, format);
    if types.is_empty() {
        "value".to_string()
    } else {
        types.join(" | ")
    }
}

/// The types a schema allows, one entry per union variant, dropping nulls and
/// duplicates.
fn type_parts(node: &Value, defs: &Value, format: Format) -> Vec<String> {
    let map_type = match format {
        Format::Toml => "table",
        Format::Json => "object",
    };
    if node.get("properties").is_some() || node.get("patternProperties").is_some() {
        return vec![map_type.to_string()];
    }
    if let Some(any) = node
        .get("anyOf")
        .or_else(|| node.get("oneOf"))
        .and_then(Value::as_array)
    {
        let mut types: Vec<String> = Vec::new();
        for variant in any {
            if variant.get("type").and_then(Value::as_str) == Some("null") {
                continue;
            }
            let (resolved, _) = resolve(variant, defs);
            for part in type_parts(resolved, defs, format) {
                if !types.contains(&part) {
                    types.push(part);
                }
            }
        }
        if !types.is_empty() {
            return types;
        }
    }
    match node.get("type") {
        Some(Value::String(s)) if s == "object" => vec![map_type.to_string()],
        Some(Value::String(s)) if s == "array" => vec![array_of(node, defs, format)],
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| *s != "null")
            .map(|s| match s {
                "array" => array_of(node, defs, format),
                "object" => map_type.to_string(),
                other => other.to_string(),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// An array schema's type, naming its element type: `array<string>`.
fn array_of(node: &Value, defs: &Value, format: Format) -> String {
    let Some(items) = node.get("items") else {
        return "array".to_string();
    };
    let (items, _) = resolve(items, defs);
    let parts = type_parts(items, defs, format);
    if parts.is_empty() {
        "array".to_string()
    } else {
        format!("array<{}>", parts.join(" | "))
    }
}
