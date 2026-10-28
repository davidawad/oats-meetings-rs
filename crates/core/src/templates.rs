//! Note templates are files: one TOML file per template in `templates.dir` (default
//! `<config dir>/templates`). The format is a config surface: closed vocabulary (an unknown field is
//! an error), versioned, and described by the generated `config/template.schema.json`.
//!
//!   schema_version = 1
//!   name = "Sales Prospect Call"
//!   icon = "person.crop.circle.badge.checkmark"     # optional
//!   system_prompt = """..."""                         # `## Heading` lines are the required sections
//!   openoats_id = "A1B2..."                          # optional, set by the importer

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{ConfigError, Result};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub schema_version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub system_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openoats_id: Option<String>,
}

fn io(p: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

pub fn parse(text: &str, origin: &str) -> Result<Template> {
    let t: Template = toml::from_str(text).map_err(|e| {
        let m = e.to_string();
        let code = if m.contains("unknown field") { "unknown_key" } else { "parse_error" };
        ConfigError::new(code, format!("template {origin}: {m}"))
    })?;
    if t.schema_version != SCHEMA_VERSION {
        return Err(ConfigError::new(
            "unsupported_version",
            format!("template {origin}: schema_version {} (this build reads {SCHEMA_VERSION})", t.schema_version),
        ));
    }
    if t.name.trim().is_empty() || t.system_prompt.trim().is_empty() {
        return Err(ConfigError::new("invalid_value", format!("template {origin}: name and system_prompt must not be empty")));
    }
    Ok(t)
}

/// All templates in `dir`, sorted by name. A missing directory is an empty list.
pub fn load_all(dir: &Path) -> Result<Vec<Template>> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Ok(vec![]);
    };
    let mut paths: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    let mut out = vec![];
    for p in paths {
        let text = std::fs::read_to_string(&p).map_err(|e| io(&p, e))?;
        out.push(parse(&text, &p.display().to_string())?);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Case-insensitive lookup by name.
pub fn find(dir: &Path, name: &str) -> Result<Template> {
    load_all(dir)?
        .into_iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .ok_or_else(|| ConfigError::new("not_found", format!("no template named {name:?} in {}", dir.display())))
}

pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-').to_string();
    if t.is_empty() { "template".into() } else { t }
}

/// The `## Heading` lines a template's prompt demands of the output.
pub fn required_headings(system_prompt: &str) -> Vec<String> {
    system_prompt
        .lines()
        .map(str::trim_end)
        .filter(|l| l.starts_with("## "))
        .map(str::to_string)
        .collect()
}

fn write_template(dir: &Path, t: &Template) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    let path = dir.join(format!("{}.toml", slug(&t.name)));
    let text = toml::to_string(t).map_err(|e| ConfigError::new("parse_error", e.to_string()))?;
    let tmp = dir.join(format!(".{}.tmp{}", slug(&t.name), std::process::id()));
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| io(&path, e))?;
    Ok(path)
}

/// Import an OpenOats `templates.json` (`{"version":1,"templates":[{name,icon,systemPrompt,id,..}]}`).
/// Idempotent; a same-named template with different content is a conflict unless `overwrite`.
pub fn import_openoats(from: &Path, dir: &Path, overwrite: bool) -> Result<Value> {
    let text = std::fs::read_to_string(from).map_err(|e| io(from, e))?;
    let v: Value = serde_json::from_str(&text)
        .map_err(|e| ConfigError::new("parse_error", format!("{}: {e}", from.display())))?;
    if v["version"] != json!(1) {
        return Err(ConfigError::new("unsupported_version", format!("{}: expected \"version\": 1", from.display())));
    }
    let items = v["templates"]
        .as_array()
        .ok_or_else(|| ConfigError::new("parse_error", format!("{}: no \"templates\" array", from.display())))?;
    let mut planned: Vec<Template> = vec![];
    for it in items {
        let s = |k: &str| it.get(k).and_then(Value::as_str).map(str::to_string);
        let (Some(name), Some(prompt)) = (s("name"), s("systemPrompt")) else {
            return Err(ConfigError::new("parse_error", format!("{}: a template lacks name or systemPrompt", from.display())));
        };
        planned.push(Template {
            schema_version: SCHEMA_VERSION,
            name,
            icon: s("icon"),
            system_prompt: prompt,
            openoats_id: s("id"),
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for t in &planned {
        if !seen.insert(slug(&t.name)) {
            return Err(ConfigError::new("conflict", format!("two templates map to the file {}.toml", slug(&t.name))));
        }
    }
    let existing = load_all(dir)?;
    let mut conflicts = vec![];
    for t in &planned {
        if let Some(e) = existing.iter().find(|e| slug(&e.name) == slug(&t.name)) {
            if e != t && !overwrite {
                conflicts.push(t.name.clone());
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(ConfigError::new(
            "conflict",
            format!("templates already present with different content: {}; use overwrite", conflicts.join(", ")),
        ));
    }
    let (mut imported, mut skipped) = (vec![], vec![]);
    for t in &planned {
        if existing.iter().any(|e| e == t) {
            skipped.push(t.name.clone());
        } else {
            write_template(dir, t)?;
            imported.push(t.name.clone());
        }
    }
    Ok(json!({"imported": imported, "skipped": skipped, "dir": dir}))
}

/// JSON Schema for the template file, generated from the struct's field list.
pub fn schema_json() -> String {
    let schema = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "$comment": "GENERATED by `oats-meetings-rs config generate` from crates/core/src/templates.rs; do not edit",
        "title": "oats-meetings-rs note template (TOML file)",
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "name", "system_prompt"],
        "properties": {
            "schema_version": {"type": "integer", "const": SCHEMA_VERSION},
            "name": {"type": "string", "minLength": 1},
            "icon": {"type": "string"},
            "system_prompt": {"type": "string", "minLength": 1, "description": "Lines starting with '## ' are the headings the output must contain (notes.enforce_headings)."},
            "openoats_id": {"type": "string"}
        }
    });
    let mut s = serde_json::to_string_pretty(&schema).expect("serializable");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same shape as the real OpenOats file, synthetic content.
    pub(crate) const SAMPLE: &str = r#"{"version":1,"templates":[
      {"name":"Sales Prospect Call","icon":"person","systemPrompt":"Notes.\n\n## CRM\nContact Name: [x]\n\n## Summary\nShort.\n\n## Next Step\nOne.","isBuiltIn":false,"id":"AAAA"},
      {"name":"1:1","systemPrompt":"## Discussion Points\n- x","isBuiltIn":false,"id":"BBBB"},
      {"name":"Stand-Up","systemPrompt":"## Yesterday\n## Today\n## Blockers"}
    ]}"#;

    #[test]
    fn slugs() {
        assert_eq!(slug("Sales Prospect Call"), "sales-prospect-call");
        assert_eq!(slug("1:1"), "1-1");
        assert_eq!(slug("Stand-Up"), "stand-up");
        assert_eq!(slug("???"), "template");
    }

    #[test]
    fn headings_are_the_exact_h2_lines() {
        let t = "intro\n## CRM\nContact\n### not this\n## Summary  \n#NotAHeading\n";
        assert_eq!(required_headings(t), vec!["## CRM".to_string(), "## Summary".to_string()]);
    }

    #[test]
    fn import_roundtrip_is_idempotent_and_conflicts_are_reported() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("templates.json");
        std::fs::write(&src, SAMPLE).unwrap();
        let dir = d.path().join("templates");
        let r = import_openoats(&src, &dir, false).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 3);
        let all = load_all(&dir).unwrap();
        let names: Vec<&str> = all.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["1:1", "Sales Prospect Call", "Stand-Up"]);
        let sales = find(&dir, "sales prospect call").unwrap();
        assert_eq!(sales.openoats_id.as_deref(), Some("AAAA"));
        assert!(required_headings(&sales.system_prompt).contains(&"## CRM".to_string()));
        // Again: nothing to do.
        let r = import_openoats(&src, &dir, false).unwrap();
        assert_eq!(r["imported"], json!([]));
        assert_eq!(r["skipped"].as_array().unwrap().len(), 3);
        // A hand edit conflicts until overwrite.
        let p = dir.join("stand-up.toml");
        let edited = std::fs::read_to_string(&p).unwrap().replace("Blockers", "Obstacles");
        std::fs::write(&p, edited).unwrap();
        assert_eq!(import_openoats(&src, &dir, false).unwrap_err().code, "conflict");
        import_openoats(&src, &dir, true).unwrap();
        assert!(find(&dir, "Stand-Up").unwrap().system_prompt.contains("Blockers"));
    }

    #[test]
    fn closed_vocabulary_and_versions() {
        let ok = "schema_version = 1\nname = \"A\"\nsystem_prompt = \"## H\"\n";
        assert!(parse(ok, "t").is_ok());
        let e = parse(&format!("{ok}extra = 1\n"), "t").unwrap_err();
        assert_eq!(e.code, "unknown_key");
        let e = parse(&ok.replace("= 1", "= 2"), "t").unwrap_err();
        assert_eq!(e.code, "unsupported_version");
        assert_eq!(parse("name = \"A\"\n", "t").unwrap_err().code, "parse_error");
        assert_eq!(parse(&ok.replace("\"A\"", "\"\""), "t").unwrap_err().code, "invalid_value");
    }

    #[test]
    fn bad_import_files() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("t.json");
        for (body, code) in [
            ("{", "parse_error"),
            (r#"{"version":2,"templates":[]}"#, "unsupported_version"),
            (r#"{"version":1}"#, "parse_error"),
            (r#"{"version":1,"templates":[{"name":"x"}]}"#, "parse_error"),
            (r#"{"version":1,"templates":[{"name":"A b","systemPrompt":"p"},{"name":"a-b","systemPrompt":"q"}]}"#, "conflict"),
        ] {
            std::fs::write(&f, body).unwrap();
            assert_eq!(import_openoats(&f, &d.path().join("o"), false).unwrap_err().code, code, "{body}");
        }
        assert_eq!(import_openoats(&d.path().join("absent.json"), &d.path().join("o"), false).unwrap_err().code, "io_error");
    }
}
