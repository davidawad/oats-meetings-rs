//! The user file's format (dot-4uzgol.87): `config.toml`, `config.json` or `config.yaml` (`.yml`),
//! one file, one schema (`config/schema.json`). Each parses into the same tree; `config-set` and
//! Settings write back in the file's own format. TOML edits go through `toml_edit`, so comments and
//! layout survive; JSON has no comments, and YAML is re-emitted (its comments are not kept). Both
//! keep the file's key order and a top-level `$schema` pointer (never read as a key).

use std::path::{Path, PathBuf};

use serde_json::Value;
use serde_yaml_ng::{Mapping, Value as Yaml};

use super::{ConfigError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Toml,
    Json,
    Yaml,
}

/// The default user file names, in discovery order.
pub const NAMES: &[&str] = &["config.toml", "config.json", "config.yaml", "config.yml"];

/// The JSON-schema pointer an editor may put at the top of a JSON or YAML file; not a key.
const SCHEMA_KEY: &str = "$schema";

impl Format {
    /// By extension: `.json`, `.yaml`/`.yml`, anything else is TOML.
    pub fn of(path: &Path) -> Format {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Format::Json,
            Some("yaml" | "yml") => Format::Yaml,
            _ => Format::Toml,
        }
    }

    /// `toml`, `json`, `yaml` or `yml` (what `config-convert --to` takes).
    pub fn named(name: &str) -> Result<Format> {
        match name
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str()
        {
            "toml" => Ok(Format::Toml),
            "json" => Ok(Format::Json),
            "yaml" | "yml" => Ok(Format::Yaml),
            other => Err(ConfigError::new(
                "invalid_value",
                format!("unknown config format '{other}'; allowed: toml, json, yaml"),
            )),
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Format::Toml => "toml",
            Format::Json => "json",
            Format::Yaml => "yaml",
        }
    }
}

/// The user file in `dir`: the one `config.{toml,json,yaml,yml}` there (`config.toml` when there is
/// none). Two or more is an error naming them, since which one wins would be a guess.
pub fn discover(dir: &Path) -> Result<PathBuf> {
    let found: Vec<PathBuf> = NAMES
        .iter()
        .map(|n| dir.join(n))
        .filter(|p| p.is_file())
        .collect();
    match found.as_slice() {
        [] => Ok(dir.join(NAMES[0])),
        [one] => Ok(one.clone()),
        many => Err(conflict(many)),
    }
}

/// The `conflicting_config` error when `path`'s directory holds another default user file beside
/// it (only for a file that is itself one of the default names).
pub fn sibling_conflict(path: &Path) -> Result<()> {
    let is_default = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| NAMES.contains(&n));
    match path.parent() {
        Some(dir) if is_default => discover(dir).map(|_| ()),
        _ => Ok(()),
    }
}

fn conflict(paths: &[PathBuf]) -> ConfigError {
    let names: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    ConfigError::new(
        "conflicting_config",
        format!(
            "more than one user config file: {}; keep one (ctl config-convert --to <format> writes one from another)",
            names.join(" and ")
        ),
    )
}

fn parse_error(e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("parse_error", format!("user config: {e}"))
}

/// The user file as an ordered tree (JSON and YAML share one; TOML comes through `toml`).
fn tree(text: &str, fmt: Format) -> Result<Yaml> {
    let mut t: Yaml = match fmt {
        Format::Toml => toml::from_str(text).map_err(parse_error)?,
        Format::Json if text.trim().is_empty() => Yaml::Mapping(Mapping::new()),
        Format::Json => serde_json::from_str(text).map_err(parse_error)?,
        Format::Yaml => serde_yaml_ng::from_str(text).map_err(parse_error)?,
    };
    if t.is_null() {
        t = Yaml::Mapping(Mapping::new()); // an empty (or comment-only) YAML file
    }
    if !t.is_mapping() {
        return Err(parse_error("the top level must be a table of keys"));
    }
    Ok(t)
}

/// Parse the user file into the nested JSON tree the resolver validates.
pub fn parse(text: &str, fmt: Format) -> Result<Value> {
    let mut t = tree(text, fmt)?;
    without_schema(&mut t);
    serde_json::to_value(t).map_err(parse_error)
}

fn without_schema(t: &mut Yaml) {
    if let Some(m) = t.as_mapping_mut() {
        m.shift_remove(SCHEMA_KEY);
    }
}

fn render_tree(t: &Yaml, fmt: Format) -> Result<String> {
    let bad =
        |e: String| ConfigError::new("type_mismatch", format!("cannot write user config: {e}"));
    match fmt {
        Format::Toml => toml::to_string(t).map_err(|e| bad(e.to_string())),
        Format::Json => serde_json::to_string_pretty(t)
            .map(|s| s + "\n")
            .map_err(|e| bad(e.to_string())),
        Format::Yaml => serde_yaml_ng::to_string(t).map_err(|e| bad(e.to_string())),
    }
}

/// The user file `text` (in `from`) rewritten in `to`, same keys, same order. Comments are dropped.
pub fn convert(text: &str, from: Format, to: Format) -> Result<String> {
    let mut t = tree(text, from)?;
    if to == Format::Toml {
        without_schema(&mut t);
    }
    render_tree(&t, to)
}

/// `text` (in `fmt`) with dotted `key` set to `value`; an empty array removes the key. TOML keeps
/// every comment and the layout around the edit.
pub fn set(text: &str, fmt: Format, key: &str, value: &Value) -> Result<String> {
    if fmt == Format::Toml {
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(parse_error)?;
        set_in_doc(&mut doc, key, value)?;
        return Ok(doc.to_string());
    }
    let mut t = tree(text, fmt)?;
    let mut new: Yaml = serde_yaml_ng::to_value(value)
        .map_err(|e| ConfigError::new("type_mismatch", format!("'{key}': {e}")))?;
    if let Yaml::Sequence(items) = &mut new {
        items.iter_mut().for_each(order_fields);
    }
    let parts: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = parts.split_last().expect("non-empty key");
    let mut cur = t.as_mapping_mut().expect("tree() returns a mapping");
    for part in parents {
        let k = Yaml::String((*part).to_string());
        if !cur.contains_key(&k) {
            cur.insert(k.clone(), Yaml::Mapping(Mapping::new()));
        }
        cur = cur
            .get_mut(&k)
            .and_then(Yaml::as_mapping_mut)
            .ok_or_else(|| {
                ConfigError::new(
                    "type_mismatch",
                    format!("'{part}' in user config is not a table"),
                )
            })?;
    }
    let k = Yaml::String((*leaf).to_string());
    if new.as_sequence().is_some_and(Vec::is_empty) {
        cur.shift_remove(&k);
    } else if let Some(old) = cur.get_mut(&k) {
        *old = new;
    } else {
        cur.insert(k, new);
    }
    render_tree(&t, fmt)
}

/// Rank of a hook field in its documented order (name, event, command, ...), the rest after.
fn field_rank(k: &str) -> usize {
    crate::hooks::FIELDS
        .iter()
        .position(|f| *f == k)
        .unwrap_or(crate::hooks::FIELDS.len())
}

fn order_fields(item: &mut Yaml) {
    if let Yaml::Mapping(m) = item {
        let mut entries: Vec<(Yaml, Yaml)> = std::mem::take(m).into_iter().collect();
        entries.sort_by_key(|(k, _)| field_rank(k.as_str().unwrap_or("")));
        m.extend(entries);
    }
}

/// Set `key` (dotted) in a TOML document, keeping every comment and the layout around it.
fn set_in_doc(doc: &mut toml_edit::DocumentMut, key: &str, value: &Value) -> Result<()> {
    use toml_edit::{Item, Table, TableLike};
    if let Value::Array(items) = value {
        return set_tables_in_doc(doc, key, items);
    }
    let new_val: toml_edit::Value = match value {
        Value::Bool(b) => (*b).into(),
        Value::String(s) => s.as_str().into(),
        Value::Number(n) if n.is_i64() => n.as_i64().unwrap_or_default().into(),
        other => {
            return Err(ConfigError::new(
                "type_mismatch",
                format!("cannot store {other} in TOML"),
            ))
        }
    };
    let parts: Vec<&str> = key.split('.').collect();
    let (leaf, parents) = parts.split_last().expect("non-empty key");
    let mut cur: &mut dyn TableLike = doc.as_table_mut();
    for part in parents {
        if !cur.contains_key(part) {
            let mut t = Table::new();
            t.set_implicit(true);
            cur.insert(part, Item::Table(t));
        }
        cur = cur
            .get_mut(part)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| {
                ConfigError::new(
                    "type_mismatch",
                    format!("'{part}' in user config is not a table"),
                )
            })?;
    }
    match cur.get_mut(leaf) {
        Some(Item::Value(old)) => {
            let mut nv = new_val;
            *nv.decor_mut() = old.decor().clone();
            *old = nv;
        }
        _ => {
            cur.insert(leaf, Item::Value(new_val));
        }
    }
    Ok(())
}

/// Set `key` to an array of tables (`[[hooks]]`), or remove it when `items` is empty.
fn set_tables_in_doc(doc: &mut toml_edit::DocumentMut, key: &str, items: &[Value]) -> Result<()> {
    use toml_edit::{ArrayOfTables, Item};
    let bad = |e: String| ConfigError::new("type_mismatch", format!("'{key}': {e}"));
    if key.contains('.') {
        return Err(bad("only a top-level key holds an array of tables".into()));
    }
    if items.is_empty() {
        doc.remove(key);
        return Ok(());
    }
    let mut aot = ArrayOfTables::new();
    for it in items {
        if !it.is_object() {
            return Err(bad(format!("{it} is not a table")));
        }
        let tv: toml::Value = serde_json::from_value(it.clone()).map_err(|e| bad(e.to_string()))?;
        let text = toml::to_string(&tv).map_err(|e| bad(e.to_string()))?;
        let one: toml_edit::DocumentMut = text.parse().map_err(|e| bad(format!("{e}")))?;
        let mut t = one.as_table().clone();
        t.sort_values_by(|a, _, b, _| field_rank(a.get()).cmp(&field_rank(b.get())));
        aot.push(t);
    }
    doc.insert(key, Item::ArrayOfTables(aot));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn format_by_extension_and_name() {
        assert_eq!(Format::of(Path::new("/x/config.yml")), Format::Yaml);
        assert_eq!(Format::of(Path::new("/x/c.json")), Format::Json);
        assert_eq!(Format::of(Path::new("/x/c")), Format::Toml);
        assert_eq!(Format::named("YML").unwrap(), Format::Yaml);
        assert_eq!(Format::named("ini").unwrap_err().code, "invalid_value");
    }

    #[test]
    fn json_and_yaml_edits_keep_key_order_and_the_schema_pointer() {
        let j = "{\"$schema\": \"x\", \"ui\": {\"z\": 1, \"a\": 2}, \"app\": {}}";
        let out = set(j, Format::Json, "ui.m", &json!(3)).unwrap();
        let (z, a, m) = (
            out.find("\"z\"").unwrap(),
            out.find("\"a\"").unwrap(),
            out.find("\"m\"").unwrap(),
        );
        assert!(
            z < a && a < m && out.find("\"ui\"") < out.find("\"app\""),
            "{out}"
        );
        assert!(out.find("$schema") < out.find("\"ui\""), "{out}");
        assert_eq!(parse(&out, Format::Json).unwrap()["ui"]["m"], 3);
        assert!(parse(&out, Format::Json).unwrap().get("$schema").is_none());
        let y = set("", Format::Yaml, "ui.theme", &json!("dark")).unwrap();
        assert_eq!(y, "ui:\n  theme: dark\n");
        assert_eq!(
            parse("# only a comment\n", Format::Yaml).unwrap(),
            json!({})
        );
        assert_eq!(
            set("ui: 1\n", Format::Yaml, "ui.theme", &json!("x"))
                .unwrap_err()
                .code,
            "type_mismatch"
        );
    }
}
