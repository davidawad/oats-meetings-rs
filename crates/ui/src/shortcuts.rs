//! Keyboard shortcuts (dot-4uzgol.82). Each command in `model::BINDINGS` has its keys in the
//! config key `ui.keys.<command>` (`oats_core::config::shortcut` has the syntax), so the map is
//! data: Settings rebinds, resets and clears it through `config-set`, and a file edit reloads it.
//!
//! Chords (ctrl, alt or cmd held) are GPUI key bindings: they work whatever has focus and the
//! menus show them. Bare keys (space, 1..7, j/k, /, ?, esc) are matched by the main window's key
//! handler instead, after a focused editor has had the key, and only while nothing is being typed
//! in, so a space typed into Notes stays a space.

use gpui::{App, Global, KeyBinding, Keystroke};
use oats_core::config::shortcut;
use oats_core::config::typed::Config;
use serde_json::{json, Map, Value};

use crate::host::Do;
use crate::model::{Binding, BINDINGS};

/// `mod` is cmd here, else ctrl.
pub const MACOS: bool = cfg!(target_os = "macos");

/// The active map: every command's keystrokes (canonical text), from `ui.keys`.
#[derive(Debug, Clone, PartialEq)]
pub struct Keymap {
    pub entries: Vec<(&'static Binding, Vec<String>)>,
}

impl Keymap {
    pub fn of(cfg: &Config) -> Self {
        Self {
            entries: BINDINGS
                .iter()
                .map(|b| {
                    let text = cfg.ui.keys.get(b.command).map_or("", String::as_str);
                    (b, shortcut::parse_list(text).unwrap_or_default())
                })
                .collect(),
        }
    }

    /// A command's keystrokes, canonical.
    pub fn keys(&self, command: &str) -> &[String] {
        self.entries
            .iter()
            .find(|(b, _)| b.command == command)
            .map_or(&[], |(_, k)| k.as_slice())
    }

    /// The command a keystroke runs; `platform` is spelled as `display` spells it.
    pub fn command_for(&self, platform: &str) -> Option<&'static str> {
        self.entries.iter().find_map(|(b, ks)| {
            ks.iter()
                .any(|k| display(k) == platform)
                .then_some(b.command)
        })
    }

    /// The chords as GPUI keystrokes, with the command each runs.
    pub fn chords(&self) -> Vec<(String, &'static str)> {
        self.entries
            .iter()
            .flat_map(|(b, ks)| {
                ks.iter()
                    .filter(|k| shortcut::is_chord(k))
                    .map(|k| (display(k), b.command))
            })
            .collect()
    }

    /// The bare-key command a key press runs, if any.
    pub fn bare(&self, k: &Keystroke) -> Option<&'static str> {
        spellings(k)
            .iter()
            .filter(|p| !shortcut::is_chord(p))
            .find_map(|p| self.command_for(p))
    }

    /// `{command: [keystroke, ...]}` as this platform spells them: what `ui-state` reports.
    pub fn json(&self) -> Value {
        let m: Map<String, Value> = self
            .entries
            .iter()
            .map(|(b, ks)| {
                let shown: Vec<String> = ks.iter().map(|k| display(k)).collect();
                (b.command.to_string(), json!(shown))
            })
            .collect();
        Value::Object(m)
    }
}

/// A canonical keystroke as this platform spells it (`mod-shift-r` is `ctrl-shift-r` on Linux).
pub fn display(canonical: &str) -> String {
    shortcut::for_platform(canonical, MACOS)
}

/// A command's keys for a label: `space / ctrl-shift-r`, or `none`.
pub fn shown(keys: &[String]) -> String {
    if keys.is_empty() {
        return "none".into();
    }
    keys.iter()
        .map(|k| display(k))
        .collect::<Vec<_>>()
        .join(" / ")
}

/// The keystroke a key event is, as canonical text (ctrl / cmd as held, not `mod`), and None for
/// a key a shortcut cannot name. An upper-case letter is shift plus the letter.
fn canonical_of(k: &Keystroke) -> Option<String> {
    let m = &k.modifiers;
    let mut parts: Vec<&str> = vec![];
    for (on, name) in [
        (m.control, "ctrl"),
        (m.alt, "alt"),
        (m.shift, "shift"),
        (m.platform, "cmd"),
    ] {
        if on {
            parts.push(name);
        }
    }
    let key = if k.key == "-" { "-" } else { k.key.as_str() };
    parts.push(key);
    shortcut::parse_one(&parts.join("-")).ok()
}

/// The character a bare key types when it differs from its key name (`?` for shift-/).
fn typed_char(k: &Keystroke) -> Option<String> {
    let m = &k.modifiers;
    if m.control || m.alt || m.platform {
        return None;
    }
    let c = k.key_char.as_deref()?;
    let mut it = c.chars();
    let (Some(ch), None) = (it.next(), it.next()) else {
        return None;
    };
    if ch.is_whitespace() || ch.is_control() || ch.is_alphabetic() || c == k.key {
        return None;
    }
    shortcut::parse_one(c).ok()
}

/// How a key event may be spelled in the map (platform spelling), most specific first.
pub fn spellings(k: &Keystroke) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for c in [canonical_of(k), typed_char(k)].into_iter().flatten() {
        let p = display(&c);
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// What Rebind stores for a pressed key: the platform's own modifier written as `mod`, so the
/// binding means the same on macOS and Linux; a shifted symbol as the character it types.
pub fn recorded(k: &Keystroke) -> Option<String> {
    if let Some(c) = typed_char(k) {
        return Some(c);
    }
    let c = canonical_of(k)?;
    let own = if MACOS { "cmd" } else { "ctrl" };
    let (mods, key) = shortcut::split(&c);
    let mut parts: Vec<&str> = mods
        .into_iter()
        .map(|m| if m == own { "mod" } else { m })
        .collect();
    parts.push(key);
    shortcut::parse_one(&parts.join("-")).ok()
}

/// The chords GPUI has bound, and whether a Rebind is listening (then none are: the pressed chord
/// must reach the window, not run its command).
#[derive(Default)]
pub struct Applied {
    chords: Option<Vec<(String, &'static str)>>,
    pub listening: bool,
}

impl Global for Applied {}

/// Bind `map`'s chords as GPUI key bindings (none while a Rebind listens) when they differ from
/// what is bound, and rebuild the menus so their shortcuts follow. True when it rebound.
pub fn sync(cx: &mut App, map: &Keymap, capture_supported: bool) -> bool {
    let want = if cx.default_global::<Applied>().listening {
        vec![]
    } else {
        map.chords()
    };
    if cx.global::<Applied>().chords.as_ref() == Some(&want) {
        return false;
    }
    cx.clear_key_bindings();
    let bindings: Vec<KeyBinding> = want
        .iter()
        .filter_map(|(k, id)| {
            KeyBinding::load(
                k,
                Box::new(Do { id: id.to_string() }),
                None,
                false,
                None,
                cx,
            )
            .ok()
        })
        .collect();
    cx.bind_keys(bindings);
    cx.set_menus(crate::host::menus(capture_supported));
    cx.global_mut::<Applied>().chords = Some(want);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(s: &str) -> Keystroke {
        // Keystroke::parse wants a 'static source.
        Keystroke::parse(Box::leak(s.to_string().into_boxed_str())).expect("a keystroke")
    }

    #[test]
    fn key_events_spell_as_the_map_does() {
        let own = if MACOS { "cmd" } else { "ctrl" };
        assert_eq!(spellings(&ks("space")), ["space"]);
        assert_eq!(
            spellings(&ks(&format!("{own}-shift-r"))),
            [display("mod-shift-r")]
        );
        assert_eq!(
            recorded(&ks(&format!("{own}-shift-r"))).unwrap(),
            "mod-shift-r"
        );
        assert_eq!(recorded(&ks("shift-s")).unwrap(), "shift-s");
        assert_eq!(recorded(&ks("1")).unwrap(), "1");
        // shift-/ types ?: it is recorded and matched as the character.
        let mut q = ks("shift-/");
        q.key_char = Some("?".into());
        assert_eq!(recorded(&q).unwrap(), "?");
        assert!(spellings(&q).contains(&"?".to_string()));
        assert_eq!(recorded(&ks("capslock")), None);
    }
}
