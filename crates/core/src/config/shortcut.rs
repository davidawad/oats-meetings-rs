//! Keyboard shortcut text for the `ui.keys.*` keys (dot-4uzgol.82): one key per window command,
//! its value a comma-separated list of keystrokes (`"mod-shift-s, s"`), empty to disable it.
//!
//! A keystroke is modifiers then a key, joined by `-`. Modifiers: `mod` (cmd on macOS, ctrl
//! elsewhere), `ctrl`, `alt`, `shift`, `cmd`. The key is one printable character (`j`, `1`, `/`,
//! `?`) or a named key (`NAMED`). An upper-case letter means shift plus the letter. A keystroke
//! with `mod`, `ctrl`, `alt` or `cmd` is a chord: it works whatever has focus. One without is a
//! bare key: the window takes it only while no text box or editor is being typed in.

use std::collections::BTreeMap;

/// Modifiers in the order the canonical text writes them.
pub const MODIFIERS: &[&str] = &["mod", "ctrl", "alt", "shift", "cmd"];

/// Key names besides single characters.
pub const NAMED: &[&str] = &[
    "space",
    "enter",
    "escape",
    "tab",
    "backspace",
    "delete",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
];

/// One keystroke in canonical form (modifiers in `MODIFIERS` order, key lower-case), or why not.
pub fn parse_one(text: &str) -> Result<String, String> {
    let s = text.trim();
    if s.is_empty() {
        return Err("an empty keystroke".into());
    }
    if s.chars().any(char::is_whitespace) {
        return Err(format!(
            "'{s}' has a space in it (one keystroke each; separate several with commas)"
        ));
    }
    let (mods, key) = if s == "-" {
        ("", "-")
    } else if let Some(m) = s.strip_suffix("--") {
        (m, "-")
    } else {
        s.rsplit_once('-').unwrap_or(("", s))
    };
    let mut have = [false; 5];
    for m in mods.split('-').filter(|m| !m.is_empty()) {
        let m = m.to_ascii_lowercase();
        let i = MODIFIERS.iter().position(|x| *x == m).ok_or_else(|| {
            format!(
                "'{s}': unknown modifier '{m}' (use {})",
                MODIFIERS.join(", ")
            )
        })?;
        if std::mem::replace(&mut have[i], true) {
            return Err(format!("'{s}': {m} twice"));
        }
    }
    let mut chars = key.chars();
    let key = match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => {
            have[3] = true;
            c.to_ascii_lowercase().to_string()
        }
        (Some(c), None) => c.to_string(),
        _ => {
            let k = key.to_ascii_lowercase();
            let k = match k.as_str() {
                "esc" => "escape".to_string(),
                "return" => "enter".to_string(),
                _ => k,
            };
            if !NAMED.contains(&k.as_str()) {
                return Err(format!(
                    "'{s}': unknown key '{key}' (one character, or one of {})",
                    NAMED.join(", ")
                ));
            }
            k
        }
    };
    let mut out: Vec<&str> = MODIFIERS
        .iter()
        .zip(have)
        .filter_map(|(m, on)| on.then_some(*m))
        .collect();
    out.push(&key);
    Ok(out.join("-"))
}

/// A `ui.keys.*` value: its keystrokes in canonical form; empty (or blank) is none (disabled).
pub fn parse_list(text: &str) -> Result<Vec<String>, String> {
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut out: Vec<String> = vec![];
    for part in text.split(',') {
        let k = parse_one(part)?;
        if out.contains(&k) {
            return Err(format!("'{k}' is listed twice"));
        }
        out.push(k);
    }
    Ok(out)
}

/// True when the keystroke holds ctrl, alt or cmd (or `mod`): it works whatever has focus.
pub fn is_chord(canonical: &str) -> bool {
    let (mods, _) = split(canonical);
    mods.iter().any(|m| *m != "shift")
}

/// The modifiers and the key of a canonical keystroke.
pub fn split(canonical: &str) -> (Vec<&str>, &str) {
    let (mods, key) = if canonical == "-" {
        ("", "-")
    } else if let Some(m) = canonical.strip_suffix("--") {
        (m, "-")
    } else {
        canonical.rsplit_once('-').unwrap_or(("", canonical))
    };
    (mods.split('-').filter(|m| !m.is_empty()).collect(), key)
}

/// The keystroke as one platform spells it: `mod` becomes cmd on macOS, ctrl elsewhere (a
/// keystroke naming both stays one of each).
pub fn for_platform(canonical: &str, macos: bool) -> String {
    let (mods, key) = split(canonical);
    let mut have: Vec<&str> = vec![];
    for m in mods {
        let m = match m {
            "mod" if macos => "cmd",
            "mod" => "ctrl",
            m => m,
        };
        if !have.contains(&m) {
            have.push(m);
        }
    }
    let mut out: Vec<&str> = ["ctrl", "alt", "shift", "cmd"]
        .into_iter()
        .filter(|m| have.contains(m))
        .collect();
    out.push(key);
    out.join("-")
}

/// Two commands on one keystroke, on either platform: the first clash as `(keystroke, a, b)`.
/// `mod-r` clashes with both `ctrl-r` and `cmd-r`, so a config valid on one OS is valid on all.
pub fn clash<'a>(
    map: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<(String, String, String)> {
    let mut seen: [BTreeMap<String, &str>; 2] = Default::default();
    for (cmd, text) in map {
        for k in parse_list(text).unwrap_or_default() {
            for (i, macos) in [false, true].into_iter().enumerate() {
                let p = for_platform(&k, macos);
                if let Some(other) = seen[i].insert(p, cmd) {
                    if other != cmd {
                        return Some((k, other.to_string(), cmd.to_string()));
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keystrokes_parse_to_one_canonical_text() {
        assert_eq!(parse_one("Shift-Mod-R").unwrap(), "mod-shift-r");
        assert_eq!(parse_one("R").unwrap(), "shift-r");
        assert_eq!(parse_one(" space ").unwrap(), "space");
        assert_eq!(parse_one("Esc").unwrap(), "escape");
        assert_eq!(parse_one("?").unwrap(), "?");
        assert_eq!(parse_one("/").unwrap(), "/");
        assert_eq!(parse_one("ctrl--").unwrap(), "ctrl--");
        assert_eq!(parse_one("-").unwrap(), "-");
        assert!(parse_one("hyper-r")
            .unwrap_err()
            .contains("unknown modifier"));
        assert!(parse_one("ctrl-ctrl-r").unwrap_err().contains("twice"));
        assert!(parse_one("ctrl-bogus").unwrap_err().contains("unknown key"));
        assert!(parse_one("ctrl-k ctrl-s").unwrap_err().contains("space"));
        assert_eq!(parse_list("j, down").unwrap(), ["j", "down"]);
        assert_eq!(parse_list("  ").unwrap(), Vec::<String>::new());
        assert!(parse_list("j, J, j").unwrap_err().contains("twice"));
    }

    #[test]
    fn chords_and_platform_spelling() {
        assert!(is_chord("mod-shift-r"));
        assert!(is_chord("alt-x"));
        assert!(!is_chord("shift-r"));
        assert!(!is_chord("space"));
        assert_eq!(for_platform("mod-shift-r", true), "shift-cmd-r");
        assert_eq!(for_platform("mod-shift-r", false), "ctrl-shift-r");
        assert_eq!(for_platform("ctrl--", false), "ctrl--");
        assert_eq!(for_platform("mod-ctrl-x", false), "ctrl-x");
    }

    #[test]
    fn a_keystroke_on_two_commands_clashes_on_either_platform() {
        assert_eq!(clash([("a", "mod-r"), ("b", "ctrl-shift-r")]), None);
        assert_eq!(
            clash([("a", "mod-r"), ("b", "ctrl-r")]),
            Some(("ctrl-r".into(), "a".into(), "b".into()))
        );
        assert!(clash([("a", "mod-r"), ("b", "cmd-r")]).is_some());
        assert!(clash([("a", "j, k"), ("b", "K")]).is_none());
        assert!(clash([("a", "j, k"), ("b", "x, k")]).is_some());
    }
}
