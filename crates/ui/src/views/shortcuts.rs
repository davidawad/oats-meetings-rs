//! The window's keyboard (dot-4uzgol.82): what a shortcut does (`MainWindow::command`), the bare
//! keys the window matches itself while nothing is being typed in, the `?` overlay listing every
//! binding, and the Settings controls that rebind (press a key), reset and clear one. The map is
//! `ui.keys` (`crate::shortcuts`); every change goes through `config-set`.

use std::sync::Arc;

use gpui::{div, prelude::*, px, rgb, rgba, AnyElement, App, Context, FontWeight, Keystroke};
use oats_core::config::keys::{self, Def};
use oats_core::config::shortcut;
use oats_core::config::ConfigError;
use serde_json::json;

use crate::kit::{self, Kind, Size};
use crate::model::{self, BINDINGS};
use crate::shortcuts::{self, Applied, Keymap};
use crate::state::{Control, EditKey, Rec, Tab};
use crate::theme::{self, MONO};
use crate::view::MainWindow;
use crate::views::{calendar::Cal, notes::Notes, sessions, settings, templates::Templates};

/// The window's keyboard state.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Keys {
    /// Whether the help overlay is up.
    pub help: bool,
    /// A Settings Rebind is waiting for a key for this command.
    pub listening: Option<&'static str>,
    /// Why the last rebind, reset or clear of a command was refused, shown on its row.
    pub note: Option<(&'static str, String)>,
}

/// A command's default keys (`ui.keys.<command>` in the key table).
pub fn default_keys(command: &str) -> &'static str {
    match keys::find(&format!("ui.keys.{command}")).map(|k| k.default) {
        Some(Def::S(s)) => s,
        _ => "",
    }
}

impl MainWindow {
    /// The shortcuts in effect (`ui.keys` of the running config).
    pub(crate) fn keymap(&self) -> Keymap {
        Keymap::of(&self.engine.snapshot().config)
    }

    /// True while a text box or editor on the shown tab takes typed keys: bare shortcuts are off.
    pub(crate) fn typing(&self, cx: &App) -> bool {
        let tab = self.state.tab;
        self.state.edit.is_some()
            || (tab == Tab::Notes && cx.try_global::<Notes>().is_some_and(|n| n.edit.is_some()))
            || (tab == Tab::Templates
                && cx
                    .try_global::<Templates>()
                    .is_some_and(|t| t.draft.is_some()))
            || (tab == Tab::Calendar && cx.try_global::<Cal>().is_some_and(|c| c.field.is_some()))
    }

    /// A key that reached the window's own handler (a focused editor did not take it): a Rebind
    /// listening, the open text box, else a bare shortcut. True when it was used.
    pub(crate) fn key_down(&mut self, k: &Keystroke, cx: &mut Context<Self>) -> bool {
        if self.keys.listening.is_some() {
            self.heard(k, cx);
            return true;
        }
        let command = k.modifiers.control || k.modifiers.alt || k.modifiers.platform;
        if self.state.edit.is_some() {
            match self.state.edit_key(&k.key, k.key_char.as_deref(), command) {
                EditKey::Commit => self.state.commit_edit(&self.engine),
                EditKey::Cancel => {
                    if !settings::escape(&mut self.state) {
                        self.state.cancel_edit();
                    }
                }
                EditKey::Handled => {}
                EditKey::Ignored => return false,
            }
            return true;
        }
        if self.typing(cx) {
            return false;
        }
        match self.keymap().bare(k) {
            Some(id) => self.command(id, cx),
            None => false,
        }
    }

    /// Press one keystroke (`ui-invoke keys.press`) as the keyboard would: a chord runs its
    /// command; a bare key only while nothing is being typed in. The command it ran, if any.
    pub(crate) fn press_keys(
        &mut self,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<Option<&'static str>, String> {
        let c = shortcut::parse_one(text)?;
        let Some(id) = self.keymap().command_for(&shortcuts::display(&c)) else {
            return Ok(None);
        };
        let chord = shortcut::is_chord(&c);
        if self.keys.listening.is_some() || (!chord && self.typing(cx)) {
            return Ok(None);
        }
        Ok(self.command(id, cx).then_some(id))
    }

    /// Run one shortcut's command. False when it does not apply here (j on the Live tab), so the
    /// key goes on to whatever else wants it.
    pub fn command(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        match id {
            "record-toggle" => {
                let c = if matches!(self.state.rec, Rec::Idle) {
                    Control::Start
                } else {
                    Control::Pause
                };
                self.press(c, cx);
            }
            "record-start" => self.press(Control::Start, cx),
            "record-pause" => self.press(Control::Pause, cx),
            "record-stop" => self.press(Control::Stop, cx),
            "session-new" => {
                if !self.new_session(cx) {
                    return false;
                }
                if self.state.tab != Tab::Live {
                    self.switch_tab(Tab::Live, cx);
                }
            }
            "help" => self.keys.help = !self.keys.help,
            "back" => return self.back(cx),
            // On Settings it focuses that tab's search bar; anywhere else the Sessions search.
            "search" if self.state.tab == Tab::Settings => {
                self.menu = None;
                settings::begin_search(&mut self.state);
            }
            "search" => {
                if self.state.tab != Tab::Sessions {
                    self.switch_tab(Tab::Sessions, cx);
                }
                let q = self.state.sessions_ui.query.clone();
                self.state.begin_edit(sessions::SEARCH, &q);
            }
            "sessions-next" | "sessions-prev" => {
                if !self.step_session(id == "sessions-next") {
                    return false;
                }
            }
            "sessions-open" => {
                let Some(sid) = self
                    .state
                    .selected
                    .clone()
                    .filter(|_| self.state.tab == Tab::Sessions)
                else {
                    return false;
                };
                cx.default_global::<Notes>().open(&self.engine, &sid);
                self.switch_tab(Tab::Notes, cx);
            }
            _ if id.starts_with("tab-") => {
                let Some(t) = Tab::shown()
                    .into_iter()
                    .find(|t| t.label().eq_ignore_ascii_case(&id["tab-".len()..]))
                else {
                    return false;
                };
                self.switch_tab(t, cx);
            }
            _ if model::binding(id).is_some_and(|b| !b.window) => self.run_action(id, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// An engine action off the UI thread (as the menus run it), then the lists reload.
    fn run_action(&mut self, id: &str, cx: &mut Context<Self>) {
        let (eng, id) = (Arc::clone(&self.engine), id.to_string());
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { model::dispatch(&eng, &id, json!({})) })
                .await;
            let _ = this.update(cx, |v, cx| {
                if let Err(e) = res {
                    v.state.status = e.to_string();
                }
                v.refresh(cx);
            });
        })
        .detach();
    }

    /// Esc: the overlay, an open menu, Recently deleted, a search, then the selection.
    fn back(&mut self, cx: &mut Context<Self>) -> bool {
        let tab = self.state.tab;
        if self.keys.help {
            self.keys.help = false;
        } else if self.menu.is_some() {
            self.menu = None;
        } else if tab == Tab::Trash {
            self.switch_tab(Tab::Sessions, cx);
        } else if tab == Tab::Settings && !self.state.settings_ui.query.is_empty() {
            settings::clear(&mut self.state);
        } else if tab == Tab::Sessions && !self.state.sessions_ui.query.is_empty() {
            self.state.sessions_ui.query.clear();
        } else if tab == Tab::Sessions && self.state.selected.is_some() {
            self.state.selected = None;
            self.state.transcript.clear();
            self.state.sessions_ui.confirm = None;
        } else {
            return false;
        }
        cx.notify();
        true
    }

    /// j / k: the next or previous session in the list as shown (the first when none is).
    fn step_session(&mut self, next: bool) -> bool {
        if self.state.tab != Tab::Sessions {
            return false;
        }
        sessions::sync_hits(&mut self.state, &self.engine);
        let ids: Vec<String> = sessions::visible(&self.state)
            .iter()
            .map(|r| r.id.clone())
            .collect();
        let Some(last) = ids.len().checked_sub(1) else {
            return false;
        };
        let at = self
            .state
            .selected
            .as_ref()
            .and_then(|s| ids.iter().position(|i| i == s));
        let i = match at {
            None => 0,
            Some(i) if next => (i + 1).min(last),
            Some(i) => i.saturating_sub(1),
        };
        self.state.sessions_ui.confirm = None;
        self.state.select(&self.engine, &ids[i]);
        true
    }

    /// Bind the chords of the map in effect (none while a Rebind listens), as a host tick would.
    fn sync_chords(&self, cx: &mut Context<Self>) {
        let map = self.keymap();
        shortcuts::sync(cx, &map, self.engine.capture_hiding_supported());
    }

    /// Settings' Rebind: the next key pressed becomes the command's shortcut. The chords are
    /// unbound meanwhile, so pressing one records it instead of running it.
    pub(crate) fn listen(&mut self, command: &'static str, cx: &mut Context<Self>) {
        self.keys = Keys {
            listening: Some(command),
            ..Keys::default()
        };
        cx.default_global::<Applied>().listening = true;
        self.sync_chords(cx);
        cx.notify();
    }

    pub(crate) fn stop_listening(&mut self, cx: &mut Context<Self>) {
        self.keys.listening = None;
        cx.default_global::<Applied>().listening = false;
        self.sync_chords(cx);
        cx.notify();
    }

    /// A key pressed while Rebind listens: Esc cancels, a key a shortcut can name is saved.
    fn heard(&mut self, k: &Keystroke, cx: &mut Context<Self>) {
        let Some(command) = self.keys.listening else {
            return;
        };
        let m = &k.modifiers;
        if k.key == "escape" && !(m.control || m.alt || m.platform || m.shift) {
            return self.stop_listening(cx);
        }
        match shortcuts::recorded(k) {
            None => {
                self.keys.note = Some((
                    command,
                    format!(
                        "{} cannot be a shortcut: press another key (Esc cancels)",
                        k.key
                    ),
                ))
            }
            Some(text) => {
                self.stop_listening(cx);
                let _ = self.rebind(command, &text, cx);
            }
        }
        cx.notify();
    }

    /// Give `command` the keys `text`, unless another command has one of them: then say which
    /// (code `conflict`) and change nothing. Written with `config-set`, so the file and the
    /// running map follow.
    pub(crate) fn rebind(
        &mut self,
        command: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), ConfigError> {
        let b = model::binding(command).ok_or_else(|| {
            ConfigError::new("bad_request", format!("no shortcut command '{command}'"))
        })?;
        let res = self.check_rebind(b.command, text).and_then(|joined| {
            model::dispatch(
                &self.engine,
                "config-set",
                json!({"key": format!("ui.keys.{}", b.command), "value": joined}),
            )
        });
        match &res {
            Ok(_) => {
                self.keys.note = None;
                self.state.refresh(&self.engine);
                self.sync_chords(cx);
            }
            Err(e) => self.keys.note = Some((b.command, e.message.clone())),
        }
        cx.notify();
        res.map(|_| ())
    }

    /// `text` as the value `command` would get, or why not: a bad keystroke, or one another
    /// command has.
    fn check_rebind(&self, command: &'static str, text: &str) -> Result<String, ConfigError> {
        let ks = shortcut::parse_list(text).map_err(|e| ConfigError::new("invalid_value", e))?;
        let joined = ks.join(", ");
        let map = self.keymap();
        let mut all: Vec<(&str, String)> = map
            .entries
            .iter()
            .filter(|(x, _)| x.command != command)
            .map(|(x, ks)| (x.command, ks.join(", ")))
            .collect();
        all.push((command, joined.clone()));
        match shortcut::clash(all.iter().map(|(c, t)| (*c, t.as_str()))) {
            Some((k, x, y)) => {
                let other = if x == command { y } else { x };
                let label = model::binding(&other).map_or("", |o| o.label);
                Err(ConfigError::new(
                    "conflict",
                    format!(
                        "{} is already \"{label}\" ({other}): rebind or clear that one first",
                        shortcuts::display(&k)
                    ),
                ))
            }
            None => Ok(joined),
        }
    }

    /// Reset: the command's default keys (refused, like a rebind, while another command has one).
    pub(crate) fn reset_keys(
        &mut self,
        command: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), ConfigError> {
        self.rebind(command, default_keys(command), cx)
    }
}

/// The help overlay: every binding by group, its keys as this platform spells them.
pub fn help(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let map = w.keymap();
    let mut panel = div()
        .id("help-panel")
        .flex()
        .flex_col()
        .gap(px(4.))
        .w(px(560.))
        .max_h(px(620.))
        .overflow_y_scroll()
        .p(px(20.))
        .rounded(px(12.))
        .border_1()
        .border_color(rgb(t.line2))
        .bg(rgb(t.surface))
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            div()
                .flex()
                .text_size(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .child("Keyboard shortcuts")
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(t.dim))
                        .font_weight(FontWeight::NORMAL)
                        .child("Rebind them in Settings · Esc closes"),
                ),
        );
    let mut group = "";
    for b in BINDINGS {
        if b.group != group {
            group = b.group;
            panel = panel.child(kit::section(group).text_color(rgb(t.dim)));
        }
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .py(px(2.))
                .child(div().flex_1().min_w_0().child(b.label))
                .child(
                    kit::mono(shortcuts::shown(map.keys(b.command)))
                        .text_size(px(12.))
                        .text_color(rgb(t.accent)),
                ),
        );
    }
    div()
        .id("help-overlay")
        .debug_selector(|| "help-overlay".into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(if t.dark { 0x000000A0 } else { 0x0E1A2B60 }))
        .on_click(cx.listener(|this, _, _, cx| {
            this.keys.help = false;
            cx.notify();
        }))
        .child(panel)
        .into_any_element()
}

/// A Settings row's control for `ui.keys.<command>`: the keys, Rebind (press a key), Reset to
/// the default, Clear (no shortcut).
pub fn control(
    w: &MainWindow,
    command: &'static str,
    keys: &[String],
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let t = theme::current();
    let listening = w.keys.listening == Some(command);
    let is_default = shortcut::parse_list(default_keys(command)).ok().as_deref() == Some(keys);
    let shown = if listening {
        div()
            .text_color(rgb(t.accent))
            .child("Press a key… (Esc cancels)")
    } else if keys.is_empty() {
        div().text_color(rgb(t.faint)).child("none")
    } else {
        div().font_family(MONO).child(shortcuts::shown(keys))
    };
    let field = kit::field(format!("keys-{command}"), listening)
        .w(px(200.))
        .flex_none()
        .text_size(px(13.))
        .child(shown);
    let rebind = kit::button(
        format!("keys-rebind-{command}"),
        "Rebind",
        Kind::Secondary,
        Size::Sm,
        !listening,
    )
    .on_click(cx.listener(move |this, _, window, cx| {
        this.listen(command, cx);
        if let Some(f) = &this.focus {
            f.focus(window, cx);
        }
    }));
    let reset = kit::button(
        format!("keys-reset-{command}"),
        "Reset",
        Kind::Secondary,
        Size::Sm,
        !is_default,
    );
    let reset = if is_default {
        reset
    } else {
        reset.on_click(cx.listener(move |this, _, _, cx| {
            let _ = this.reset_keys(command, cx);
        }))
    };
    let clear = kit::button(
        format!("keys-clear-{command}"),
        "Clear",
        Kind::Secondary,
        Size::Sm,
        !keys.is_empty(),
    );
    let clear = if keys.is_empty() {
        clear
    } else {
        clear.on_click(cx.listener(move |this, _, _, cx| {
            let _ = this.rebind(command, "", cx);
        }))
    };
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(field)
        .child(rebind)
        .child(reset)
        .child(clear)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;
    use gpui::{TestAppContext, VisualTestContext};
    use oats_core::control::engine::Engine;
    use serde_json::Value;

    fn seed(dir: &std::path::Path, ids: &[&str]) {
        for (i, id) in ids.iter().enumerate() {
            let s = dir.join("sessions").join(id);
            std::fs::create_dir_all(&s).unwrap();
            std::fs::write(
                s.join("session.json"),
                format!(r#"{{"id":"{id}","title":"T {id}","startedAt":"2026-10-2{i}T10:00:00Z"}}"#),
            )
            .unwrap();
            std::fs::write(
                s.join("transcript.final.jsonl"),
                "{\"speaker\":\"you\",\"text\":\"hello\"}\n",
            )
            .unwrap();
        }
    }

    /// The main window with the chords bound as the host binds them, and keyboard focus in it.
    fn window<'a>(
        engine: &Arc<Engine>,
        cx: &'a mut TestAppContext,
    ) -> (gpui::Entity<MainWindow>, &'a mut VisualTestContext) {
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        cx.update(|_, cx| {
            shortcuts::sync(cx, &Keymap::of(&engine.snapshot().config), false);
        });
        cx.run_until_parked();
        (view, cx)
    }

    fn keys(cx: &mut VisualTestContext, k: &str) {
        cx.simulate_keystrokes(k);
        cx.run_until_parked();
    }

    fn rec(engine: &Engine) -> &'static str {
        match engine.recording().flatten() {
            None => "idle",
            Some((_, true, _)) => "paused",
            Some(_) => "recording",
        }
    }

    fn dump(view: &gpui::Entity<MainWindow>, cx: &mut VisualTestContext) -> Value {
        view.update(cx, |v, cx| v.dump(cx))
    }

    /// A chord with this platform's own modifier (what `mod` means here).
    fn own(chord: &str) -> String {
        format!("{}-{chord}", if shortcuts::MACOS { "cmd" } else { "ctrl" })
    }

    #[test]
    fn space_starts_pauses_and_resumes_and_s_stops() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        keys(cx, "space");
        assert_eq!(rec(&engine), "recording");
        assert_eq!(dump(&view, cx)["recording"]["state"], "recording");
        keys(cx, "space");
        assert_eq!(rec(&engine), "paused");
        keys(cx, "space");
        assert_eq!(rec(&engine), "recording");
        // The chord pauses too; s stops. (One start per test: session ids are per second.)
        keys(cx, &own("shift-p"));
        assert_eq!(rec(&engine), "paused");
        keys(cx, "space");
        assert_eq!(rec(&engine), "recording");
        keys(cx, "s");
        assert_eq!(rec(&engine), "idle");
        assert_eq!(dump(&view, cx)["recording"]["state"], "idle");
    }

    #[test]
    fn digits_switch_tabs_in_rail_order() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        let rail: Vec<Tab> = Tab::RAIL.iter().flat_map(|g| g.iter().copied()).collect();
        assert_eq!(rail.len(), 7);
        for (i, t) in rail.iter().enumerate().rev() {
            keys(cx, &(i + 1).to_string());
            view.read_with(cx, |v, _| assert_eq!(v.state.tab, *t, "key {}", i + 1));
            assert_eq!(dump(&view, cx)["tab"], t.label());
        }
        // ui-state reports the active map, as this platform spells it.
        let d = dump(&view, cx);
        assert_eq!(d["keys"]["map"]["tab-live"], json!(["1"]));
        assert_eq!(d["keys"]["map"]["record-toggle"], json!(["space"]));
        assert_eq!(
            d["keys"]["map"]["record-stop"],
            json!([shortcuts::display("mod-shift-s"), "s"])
        );
    }

    #[test]
    fn a_space_typed_into_notes_is_a_space_not_a_recording() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), &["s1"]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        view.update(cx, |v, cx| {
            v.state.refresh(&v.engine);
            v.state.select(&v.engine, "s1");
            v.switch_tab(Tab::Notes, cx);
        });
        cx.run_until_parked();
        view.update(cx, |_, cx| {
            cx.global_mut::<Notes>().begin_edit();
            cx.notify();
        });
        cx.run_until_parked();
        let focus = view.update(cx, |_, cx| cx.global::<Notes>().focus.clone().unwrap());
        cx.update(|window, cx| focus.focus(window, cx));
        keys(cx, "a space 1 s ? j");
        view.update(cx, |v, cx| {
            assert_eq!(cx.global::<Notes>().edit.as_deref(), Some("a 1s?j"));
            assert_eq!(v.state.tab, Tab::Notes);
            assert!(!v.keys.help);
        });
        assert_eq!(rec(&engine), "idle");
        // Arrow keys and Esc are the editor's too; Esc closes it, then space records.
        keys(cx, "down escape");
        view.update(cx, |_, cx| assert_eq!(cx.global::<Notes>().edit, None));
        keys(cx, "space");
        assert_eq!(rec(&engine), "recording");
    }

    #[test]
    fn typing_in_a_settings_or_search_box_never_runs_a_bare_key() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), &["s1"]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        view.update(cx, |v, cx| {
            v.switch_tab(Tab::Settings, cx);
            v.state.begin_edit("ui.tz", "");
        });
        keys(cx, "1 space ?");
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.tab, Tab::Settings);
            assert_eq!(v.state.edit.as_ref().unwrap().buf, "1 ?");
            assert!(!v.keys.help);
        });
        assert_eq!(rec(&engine), "idle");
        keys(cx, "escape");
        // A chord still works while a box is open: mod-shift-r starts, mod-shift-s stops.
        view.update(cx, |v, _| v.state.begin_edit("ui.tz", ""));
        keys(cx, &own("shift-r"));
        assert_eq!(rec(&engine), "recording");
        keys(cx, &own("shift-s"));
        assert_eq!(rec(&engine), "idle");
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.edit.as_ref().unwrap().buf, "")
        });
    }

    #[test]
    fn sessions_keys_move_open_search_and_back_out() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), &["a", "b", "c"]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        view.update(cx, |v, cx| v.refresh(cx));
        keys(cx, "2");
        let order: Vec<String> = view.read_with(cx, |v, _| {
            sessions::visible(&v.state)
                .iter()
                .map(|r| r.id.clone())
                .collect()
        });
        assert_eq!(order.len(), 3);
        let sel = |view: &gpui::Entity<MainWindow>, cx: &mut VisualTestContext| {
            view.read_with(cx, |v, _| v.state.selected.clone())
        };
        keys(cx, "j");
        assert_eq!(sel(&view, cx).as_ref(), Some(&order[0]));
        keys(cx, "j down");
        assert_eq!(sel(&view, cx).as_ref(), Some(&order[2]));
        keys(cx, "j");
        assert_eq!(
            sel(&view, cx).as_ref(),
            Some(&order[2]),
            "stays on the last"
        );
        keys(cx, "k up");
        assert_eq!(sel(&view, cx).as_ref(), Some(&order[0]));
        view.read_with(cx, |v, _| assert_eq!(v.state.transcript.len(), 1));
        // / opens the search box; what follows is typed into it, Enter applies it.
        keys(cx, "/ T space b enter");
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.sessions_ui.query, "T b");
            assert!(v.state.edit.is_none());
            assert_eq!(sessions::visible(&v.state).len(), 1);
        });
        // Esc backs out: the search, then the selection.
        keys(cx, "escape");
        view.read_with(cx, |v, _| assert_eq!(v.state.sessions_ui.query, ""));
        keys(cx, "escape");
        assert_eq!(sel(&view, cx), None);
        // Enter opens the selected session's notes.
        keys(cx, "j enter");
        view.update(cx, |v, cx| {
            assert_eq!(v.state.tab, Tab::Notes);
            assert_eq!(cx.global::<Notes>().session.as_ref(), Some(&order[0]));
        });
        // j off the Sessions tab does nothing.
        keys(cx, "j");
        view.read_with(cx, |v, _| assert_eq!(v.state.tab, Tab::Notes));
    }

    #[test]
    fn question_mark_shows_every_binding_and_esc_closes_it() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        keys(cx, "?");
        assert_eq!(dump(&view, cx)["keys"]["help"], true);
        assert!(cx.debug_bounds("help-overlay").is_some());
        keys(cx, "escape");
        assert_eq!(dump(&view, cx)["keys"]["help"], false);
        assert!(cx.debug_bounds("help-overlay").is_none());
        // ? typed as shift-/ with its character.
        let mut q = Keystroke::parse("shift-/").expect("a keystroke");
        q.key_char = Some("?".into());
        let w = cx.window_handle();
        cx.dispatch_keystroke(w, q);
        cx.run_until_parked();
        assert_eq!(dump(&view, cx)["keys"]["help"], true);
        keys(cx, "?");
        assert_eq!(dump(&view, cx)["keys"]["help"], false);
    }

    #[test]
    fn rebind_by_pressing_a_key_refuses_a_clash_and_reset_and_clear_write_the_file() {
        let (engine, dir) = testutil::engine(&[]);
        let file = dir.path().join("config.toml");
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        keys(cx, "7");
        // Rebind tab-live by pressing space: record-toggle has it, so nothing changes.
        view.update(cx, |v, cx| v.listen("tab-live", cx));
        keys(cx, "space");
        view.read_with(cx, |v, _| {
            assert_eq!(v.keys.listening, None);
            let (c, why) = v.keys.note.clone().unwrap();
            assert_eq!(c, "tab-live");
            assert!(why.contains("record-toggle"), "{why}");
        });
        assert_eq!(rec(&engine), "idle", "the pressed key did not run");
        assert_eq!(engine.snapshot().config.ui.keys["tab-live"], "1");
        // Press 0 instead: saved to the file and live at once.
        view.update(cx, |v, cx| v.listen("tab-live", cx));
        keys(cx, "0");
        assert_eq!(engine.snapshot().config.ui.keys["tab-live"], "0");
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains("tab-live = \"0\""));
        keys(cx, "1");
        view.read_with(cx, |v, _| assert_eq!(v.state.tab, Tab::Settings));
        keys(cx, "0");
        view.read_with(cx, |v, _| assert_eq!(v.state.tab, Tab::Live));
        // A chord pressed while listening is recorded, not run, and bound after.
        view.update(cx, |v, cx| v.listen("record-start", cx));
        keys(cx, &own("alt-r"));
        assert_eq!(rec(&engine), "idle");
        assert_eq!(
            engine.snapshot().config.ui.keys["record-start"],
            "mod-alt-r"
        );
        keys(cx, &own("shift-r"));
        assert_eq!(rec(&engine), "idle", "the old chord is gone");
        keys(cx, &own("alt-r"));
        assert_eq!(rec(&engine), "recording");
        keys(cx, "s");
        // Esc cancels a Rebind.
        view.update(cx, |v, cx| v.listen("help", cx));
        keys(cx, "escape");
        assert_eq!(engine.snapshot().config.ui.keys["help"], "?");
        // Reset and Clear.
        view.update(cx, |v, cx| {
            v.reset_keys("tab-live", cx).unwrap();
            v.rebind("help", "", cx).unwrap();
        });
        assert_eq!(engine.snapshot().config.ui.keys["tab-live"], "1");
        keys(cx, "7 ?");
        view.read_with(cx, |v, _| assert!(!v.keys.help, "help has no key now"));
        let d = dump(&view, cx);
        assert_eq!(d["keys"]["map"]["help"], json!([]));
        // The Settings section lists every binding.
        let group = d["view"]["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["name"] == "Keyboard shortcuts")
            .cloned()
            .unwrap();
        assert_eq!(group["rows"].as_array().unwrap().len(), BINDINGS.len());
    }

    #[test]
    fn an_edited_config_file_rebinds_without_a_restart() {
        let (engine, dir) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        std::fs::write(
            dir.path().join("config.toml"),
            "[ui.keys]\nrecord-toggle = \"r\"\nrecord-pause = \"mod-alt-x\"\n",
        )
        .unwrap();
        engine.reload("test").unwrap();
        // What the host tick does each round.
        cx.update(|_, cx| {
            assert!(shortcuts::sync(
                cx,
                &Keymap::of(&engine.snapshot().config),
                false
            ));
        });
        keys(cx, "space");
        assert_eq!(rec(&engine), "idle");
        keys(cx, "r");
        assert_eq!(rec(&engine), "recording");
        keys(cx, &own("shift-p"));
        assert_eq!(rec(&engine), "recording", "the old chord is gone");
        keys(cx, &own("alt-x"));
        assert_eq!(rec(&engine), "paused");
        assert_eq!(
            dump(&view, cx)["keys"]["map"]["record-toggle"],
            json!(["r"])
        );
    }

    #[test]
    fn ui_invoke_presses_keys_and_rebinds() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        let invoke = |view: &gpui::Entity<MainWindow>,
                      cx: &mut VisualTestContext,
                      name: &str,
                      args: Value| {
            let r = view.update(cx, |v, cx| {
                v.drive(&json!({"op": "invoke", "name": name, "args": args}), cx)
            });
            cx.run_until_parked();
            r
        };
        let press = |view: &gpui::Entity<MainWindow>, cx: &mut VisualTestContext, k: &str| {
            invoke(view, cx, "keys.press", json!({"keys": k})).unwrap()
        };
        press(&view, cx, "space");
        assert_eq!(rec(&engine), "recording");
        press(&view, cx, "space");
        assert_eq!(rec(&engine), "paused");
        press(&view, cx, "space");
        assert_eq!(rec(&engine), "recording");
        let d = press(&view, cx, "3");
        assert_eq!(d["tab"], "Notes");
        assert_eq!(d["keys"]["pressed"]["command"], "tab-notes");
        press(&view, cx, "mod-shift-s");
        assert_eq!(rec(&engine), "idle");
        let e = invoke(
            &view,
            cx,
            "keys.rebind",
            json!({"command": "tab-live", "keys": "space"}),
        )
        .unwrap_err();
        assert_eq!(e.code, "conflict");
        invoke(
            &view,
            cx,
            "keys.rebind",
            json!({"command": "tab-live", "keys": "0, F1"}),
        )
        .unwrap();
        assert_eq!(engine.snapshot().config.ui.keys["tab-live"], "0, f1");
        invoke(&view, cx, "keys.reset", json!({"command": "tab-live"})).unwrap();
        invoke(&view, cx, "keys.clear", json!({"command": "quit"})).unwrap();
        let d = invoke(&view, cx, "keys.press", json!({"keys": "1"})).unwrap();
        assert_eq!(d["tab"], "Live");
        assert_eq!(d["keys"]["map"]["quit"], json!([]));
    }

    #[test]
    fn new_session_is_enabled_only_when_idle_with_a_finished_session_shown() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        let invoke = |view: &gpui::Entity<MainWindow>, cx: &mut VisualTestContext, name: &str| {
            view.update(cx, |v, cx| {
                v.drive(&json!({"op": "invoke", "name": name, "args": {}}), cx)
            })
            .unwrap();
            // A record press runs off the UI thread: the dump after it lands.
            cx.run_until_parked();
            dump(view, cx)
        };
        // Idle with nothing shown: nothing to clear.
        let d = dump(&view, cx);
        assert_eq!(d["recording"]["new_session"], false);
        assert_eq!(
            invoke(&view, cx, "session.new")["view"]["session"],
            Value::Null
        );
        // Recording and paused: off, and pressing it changes nothing.
        let d = invoke(&view, cx, "record.start");
        assert_eq!(d["recording"]["state"], "recording");
        assert_eq!(d["recording"]["new_session"], false);
        let session = d["recording"]["session"].as_str().unwrap().to_string();
        assert_eq!(invoke(&view, cx, "session.new")["view"]["session"], session);
        let d = invoke(&view, cx, "record.pause");
        assert_eq!(d["recording"]["state"], "paused");
        assert_eq!(d["recording"]["new_session"], false);
        assert_eq!(invoke(&view, cx, "session.new")["view"]["session"], session);
        // Saved: Live keeps the finished session until New session.
        let d = invoke(&view, cx, "record.stop");
        assert_eq!(d["recording"]["state"], "idle");
        view.update(cx, |v, cx| {
            v.event(
                &json!({"event": "recording-stopped", "data": {"session": session}}).to_string(),
                cx,
            );
            v.state.live.push(crate::state::Line {
                speaker: "you".into(),
                text: "hello".into(),
                start: None,
            });
            v.switch_tab(Tab::Sessions, cx);
        });
        let d = dump(&view, cx);
        assert_eq!(d["recording"]["new_session"], true);
        assert_eq!(d["status"], format!("Saved session {session}"));
        let d = invoke(&view, cx, "session.new");
        assert_eq!(d["tab"], "Live");
        assert_eq!(d["view"]["session"], Value::Null);
        assert_eq!(d["view"]["lines"], json!([]));
        assert_eq!(d["status"], "");
        assert_eq!(d["recording"]["state"], "idle");
        assert_eq!(d["recording"]["start"], true);
        assert_eq!(d["recording"]["new_session"], false);
        // The finished session is still in Sessions.
        view.update(cx, |v, _| {
            v.state.refresh(&v.engine);
            assert!(
                v.state.sessions.iter().any(|r| r.id == session),
                "{session}"
            );
        });
    }

    #[test]
    fn n_is_new_session_and_rebindable() {
        let (engine, _d) = testutil::engine(&[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = window(&engine, &mut cx);
        assert_eq!(
            dump(&view, cx)["keys"]["map"]["session-new"],
            json!([shortcuts::display("mod-shift-n"), "n"])
        );
        let shown = |view: &gpui::Entity<MainWindow>, cx: &mut VisualTestContext| {
            view.update(cx, |v, _| {
                v.state.live_session = Some("old".into());
                v.state.status = "Saved session old".into();
            });
        };
        shown(&view, cx);
        view.update(cx, |v, cx| v.switch_tab(Tab::Settings, cx));
        keys(cx, "n");
        let d = dump(&view, cx);
        assert_eq!(d["tab"], "Live");
        assert_eq!(d["view"]["session"], Value::Null);
        shown(&view, cx);
        keys(cx, &own("shift-n"));
        assert_eq!(dump(&view, cx)["view"]["session"], Value::Null);
        // Rebound: the old bare key does nothing, the new one clears.
        view.update(cx, |v, cx| v.rebind("session-new", "mod-alt-n", cx))
            .unwrap();
        cx.run_until_parked();
        shown(&view, cx);
        keys(cx, "n");
        assert_eq!(dump(&view, cx)["view"]["session"], "old");
        keys(cx, &own("alt-n"));
        assert_eq!(dump(&view, cx)["view"]["session"], Value::Null);
    }
}
