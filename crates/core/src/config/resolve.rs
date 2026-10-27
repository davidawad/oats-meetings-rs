//! The router: `(defaults, axes, pack, user layer) -> resolved config`. A pure function of its
//! inputs, so switching packs can never leak a stale flag (exclusive override).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use super::keys::{self, Kind, KEYS};
use super::typed::{Config, SttAdapter, SttBackend};
use super::{ConfigError, Result};

pub type Flat = BTreeMap<String, Value>;

pub const PACKS: &[(&str, &str)] = &[
    ("cloud", include_str!("../../../../config/packs/cloud.json")),
    (
        "local-only",
        include_str!("../../../../config/packs/local-only.json"),
    ),
];
pub const AXES_JSON: &str = include_str!("../../../../config/axes.json");

pub fn flatten(v: &Value, prefix: &str, out: &mut Flat) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let p = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(x, &p, out);
            }
        }
        leaf => {
            out.insert(prefix.to_string(), leaf.clone());
        }
    }
}

pub fn nest(flat: &Flat) -> Value {
    let mut root = Map::new();
    for (path, v) in flat {
        let parts: Vec<&str> = path.split('.').collect();
        let mut cur = &mut root;
        for (i, part) in parts.iter().enumerate() {
            if i + 1 == parts.len() {
                cur.insert((*part).to_string(), v.clone());
            } else {
                let next = cur
                    .entry((*part).to_string())
                    .or_insert_with(|| Value::Object(Map::new()));
                cur = next.as_object_mut().expect("path prefix is an object");
            }
        }
    }
    Value::Object(root)
}

pub fn defaults_flat() -> Flat {
    KEYS.iter()
        .map(|k| (k.path.to_string(), k.default.to_json()))
        .collect()
}

/// Closed vocabulary + type fidelity for one layer.
pub fn validate_layer(flat: &Flat, layer: &str) -> Result<()> {
    for (path, v) in flat {
        let key = keys::find(path).ok_or_else(|| {
            ConfigError::new("unknown_key", format!("unknown key '{path}' in {layer}"))
        })?;
        match (key.kind, v) {
            (Kind::Bool, Value::Bool(_)) | (Kind::Str, Value::String(_)) => {}
            (Kind::Enum(vals), Value::String(s)) => {
                if !vals.contains(&s.as_str()) {
                    return Err(ConfigError::new(
                        "invalid_value",
                        format!(
                            "'{path}' = \"{s}\" in {layer}; allowed: {}",
                            vals.join(", ")
                        ),
                    ));
                }
            }
            (kind, other) => {
                let want = match kind {
                    Kind::Bool => "boolean",
                    _ => "string",
                };
                return Err(ConfigError::new(
                    "type_mismatch",
                    format!("'{path}' in {layer} must be a {want}, got {other}"),
                ));
            }
        }
    }
    Ok(())
}

fn parse_json_layer(text: &str, layer: &str) -> Result<Flat> {
    let v: Value = serde_json::from_str(text)
        .map_err(|e| ConfigError::new("parse_error", format!("{layer}: {e}")))?;
    let mut f = Flat::new();
    flatten(&v, "", &mut f);
    Ok(f)
}

pub fn pack_names() -> Vec<&'static str> {
    PACKS.iter().map(|(n, _)| *n).collect()
}

pub fn pack_flat(name: &str) -> Result<Flat> {
    let (_, text) = PACKS.iter().find(|(n, _)| *n == name).ok_or_else(|| {
        ConfigError::new(
            "unknown_pack",
            format!("unknown pack '{name}'; known: {}", pack_names().join(", ")),
        )
    })?;
    let f = parse_json_layer(text, &format!("pack {name}"))?;
    validate_layer(&f, &format!("pack {name}"))?;
    Ok(f)
}

/// Every key any pack manages.
pub fn managed_keys() -> Result<BTreeSet<String>> {
    let mut s = BTreeSet::new();
    for n in pack_names() {
        s.extend(pack_flat(n)?.into_keys());
    }
    Ok(s)
}

/// axes.json: {"os": {"linux": {...}}, "arch": {...}, "machine": {...}}.
pub fn axis_flat(axis: &str, value: &str) -> Result<Flat> {
    let all: Value = serde_json::from_str(AXES_JSON)
        .map_err(|e| ConfigError::new("parse_error", format!("axes.json: {e}")))?;
    let mut f = Flat::new();
    if let Some(layer) = all.get(axis).and_then(|a| a.get(value)) {
        flatten(layer, "", &mut f);
    }
    validate_layer(&f, &format!("axis {axis}={value}"))?;
    Ok(f)
}

/// Check every axis entry in axes.json, not just the ones this host selects.
pub fn validate_all_axes() -> Result<()> {
    let all: Value = serde_json::from_str(AXES_JSON)
        .map_err(|e| ConfigError::new("parse_error", format!("axes.json: {e}")))?;
    let obj = all
        .as_object()
        .ok_or_else(|| ConfigError::new("parse_error", "axes.json must be an object"))?;
    for (axis, vals) in obj {
        if !matches!(axis.as_str(), "os" | "arch" | "machine") {
            return Err(ConfigError::new(
                "unknown_key",
                format!("unknown axis '{axis}' in axes.json"),
            ));
        }
        for (val, layer) in vals.as_object().into_iter().flatten() {
            let mut f = Flat::new();
            flatten(layer, "", &mut f);
            validate_layer(&f, &format!("axis {axis}={val}"))?;
        }
    }
    Ok(())
}

pub struct Inputs<'a> {
    pub pack: Option<&'a str>,
    pub os: &'a str,
    pub arch: &'a str,
    pub machine: &'a str,
    /// Contents of the user TOML file, if any.
    pub user_toml: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub pack: Option<String>,
    pub flat: Flat,
    /// Which layer set each key's final value.
    pub provenance: BTreeMap<String, String>,
}

impl Resolved {
    pub fn nested(&self) -> Value {
        nest(&self.flat)
    }

    pub fn typed(&self) -> Result<Config> {
        let cfg: Config = serde_json::from_value(self.nested())
            .map_err(|e| ConfigError::new("type_mismatch", e.to_string()))?;
        if cfg.stt.backend == SttBackend::Cloud && cfg.stt.cloud.adapter == SttAdapter::None {
            return Err(ConfigError::new(
                "invalid_combination",
                "stt.backend = \"cloud\" needs stt.cloud.adapter other than \"none\"",
            ));
        }
        Ok(cfg)
    }
}

pub fn resolve(inp: &Inputs) -> Result<Resolved> {
    let mut flat = defaults_flat();
    let mut prov: BTreeMap<String, String> =
        flat.keys().map(|k| (k.clone(), "default".into())).collect();
    let mut apply = |layer: Flat, name: String, flat: &mut Flat| {
        for (k, v) in layer {
            prov.insert(k.clone(), name.clone());
            flat.insert(k, v);
        }
    };
    for (axis, val) in [("os", inp.os), ("arch", inp.arch), ("machine", inp.machine)] {
        apply(axis_flat(axis, val)?, format!("axis:{axis}={val}"), &mut flat);
    }
    if let Some(p) = inp.pack {
        apply(pack_flat(p)?, format!("pack:{p}"), &mut flat);
    }
    if let Some(text) = inp.user_toml {
        let tv: toml::Value = toml::from_str(text)
            .map_err(|e| ConfigError::new("parse_error", format!("user config: {e}")))?;
        let jv = serde_json::to_value(tv)
            .map_err(|e| ConfigError::new("parse_error", format!("user config: {e}")))?;
        let mut f = Flat::new();
        flatten(&jv, "", &mut f);
        validate_layer(&f, "user config")?;
        apply(f, "user".into(), &mut flat);
    }
    let r = Resolved {
        pack: inp.pack.map(str::to_string),
        flat,
        provenance: prov,
    };
    r.typed()?;
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base<'a>() -> Inputs<'a> {
        Inputs {
            pack: None,
            os: "linux",
            arch: "x86_64",
            machine: "default",
            user_toml: None,
        }
    }

    fn with_user(toml: &str) -> Result<Resolved> {
        resolve(&Inputs {
            user_toml: Some(toml),
            ..base()
        })
    }

    #[test]
    fn defaults_resolve_and_type() {
        let r = resolve(&base()).unwrap();
        let c = r.typed().unwrap();
        assert_eq!(c.license.spdx, "LicenseRef-Undecided");
    }

    #[test]
    fn unknown_key_is_an_error() {
        let e = with_user("[app]\nmenubr = true\n").unwrap_err();
        assert_eq!(e.code, "unknown_key");
        let e = with_user("nonsense = 1\n").unwrap_err();
        assert_eq!(e.code, "unknown_key");
    }

    #[test]
    fn type_and_value_errors() {
        assert_eq!(
            with_user("[app]\nmenubar = \"yes\"\n").unwrap_err().code,
            "type_mismatch"
        );
        assert_eq!(
            with_user("[stt]\nbackend = \"quantum\"\n").unwrap_err().code,
            "invalid_value"
        );
        assert_eq!(with_user("[app\n").unwrap_err().code, "parse_error");
    }

    #[test]
    fn cloud_stt_needs_an_adapter() {
        let e = with_user("[stt]\nbackend = \"cloud\"\n").unwrap_err();
        assert_eq!(e.code, "invalid_combination");
    }

    /// Per-value test for every choice key: every allowed value resolves, lands in the
    /// resolved config and in the typed view; any other value is rejected.
    #[test]
    fn every_choice_key_works_for_every_value() {
        for (path, values) in keys::choices() {
            for v in &values {
                let lit = if v == "true" || v == "false" {
                    v.clone()
                } else {
                    format!("\"{v}\"")
                };
                // Keys that only make sense together are set together.
                let extra = if path == "stt.backend" && v == "cloud" {
                    "\n[stt.cloud]\nadapter = \"openai-compatible\"\n"
                } else {
                    ""
                };
                let (table, leaf) = path.rsplit_once('.').unwrap();
                let toml = format!("[{table}]\n{leaf} = {lit}\n{extra}");
                let r = with_user(&toml).unwrap_or_else(|e| panic!("{path}={v}: {e}"));
                let got = &r.flat[path];
                let want: Value = serde_json::from_str(&lit).unwrap();
                assert_eq!(got, &want, "{path}={v}");
                r.typed().unwrap();
            }
            let k = keys::find(path).unwrap();
            if let Kind::Enum(_) = k.kind {
                let (table, leaf) = path.rsplit_once('.').unwrap();
                let e = with_user(&format!("[{table}]\n{leaf} = \"__nope__\"\n")).unwrap_err();
                assert_eq!(e.code, "invalid_value", "{path}");
            }
        }
    }

    #[test]
    fn every_non_choice_key_takes_a_string() {
        for k in KEYS.iter().filter(|k| k.kind == Kind::Str) {
            let (table, leaf) = k.path.rsplit_once('.').unwrap();
            let r = with_user(&format!("[{table}]\n{leaf} = \"x-value\"\n")).unwrap();
            assert_eq!(r.flat[k.path], Value::String("x-value".into()));
        }
    }

    #[test]
    fn every_key_is_covered_by_the_typed_view() {
        // Typed deserialization has deny_unknown_fields and no defaults: a key in KEYS
        // that is missing from `Config` (or the reverse) fails here.
        let r = resolve(&base()).unwrap();
        let back = serde_json::to_value(r.typed().unwrap()).unwrap();
        assert_eq!(back, r.nested());
    }

    #[test]
    fn packs_are_closed_and_exclusive() {
        for n in pack_names() {
            pack_flat(n).unwrap();
        }
        validate_all_axes().unwrap();
        let managed = managed_keys().unwrap();
        // Every pack sets every managed key: switching packs resets all of them.
        for n in pack_names() {
            let keys: BTreeSet<String> = pack_flat(n).unwrap().into_keys().collect();
            assert_eq!(keys, managed, "pack {n} must set all managed keys");
        }
        let a = resolve(&Inputs { pack: Some("cloud"), ..base() }).unwrap();
        let b = resolve(&Inputs { pack: Some("local-only"), ..base() }).unwrap();
        let d = resolve(&base()).unwrap();
        assert_ne!(a.flat, b.flat);
        // local-only equals defaults: no stale flag from a previous pack.
        assert_eq!(b.flat, d.flat);
        let diff: BTreeSet<String> = d
            .flat
            .iter()
            .filter(|(k, v)| a.flat.get(*k) != Some(*v))
            .map(|(k, _)| k.clone())
            .collect();
        assert!(!diff.is_empty());
        assert!(diff.is_subset(&managed), "{diff:?} not within {managed:?}");
    }

    #[test]
    fn unknown_pack() {
        let e = resolve(&Inputs { pack: Some("nope"), ..base() }).unwrap_err();
        assert_eq!(e.code, "unknown_pack");
    }

    #[test]
    fn os_axis_sets_hide_method_and_user_wins() {
        for (os, want) in [
            ("macos", "sharing_type"),
            ("windows", "display_affinity"),
            ("linux", "none"),
        ] {
            let r = resolve(&Inputs { os, ..base() }).unwrap();
            assert_eq!(r.flat["privacy.hide_method"], Value::String(want.into()), "{os}");
        }
        let r = resolve(&Inputs {
            os: "macos",
            user_toml: Some("[privacy]\nhide_method = \"none\"\n"),
            ..base()
        })
        .unwrap();
        assert_eq!(r.flat["privacy.hide_method"], Value::String("none".into()));
        assert_eq!(r.provenance["privacy.hide_method"], "user");
    }

    #[test]
    fn licence_is_a_placeholder_by_default() {
        let c = resolve(&base()).unwrap().typed().unwrap();
        assert_eq!(c.license.spdx, "LicenseRef-Undecided");
        assert_eq!(c.license.holder, "");
    }
}
