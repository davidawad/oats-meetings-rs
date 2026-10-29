//! Sessions tab: the session list (searchable) and the selected session's transcript, with rename,
//! tag, delete and (for one saved without a transcript) transcribe through the `session-*` actions
//! `ctl` uses. Text inputs reuse the window's single
//! `Edit` under the `session:*` paths; `commit` is where Enter / save lands for them.

use std::sync::Arc;

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, SharedString, Window};
use oats_core::config::typed::DeleteMode;
use oats_core::control::engine::Engine;
use serde_json::json;

use crate::model;
use crate::state::{Edit, SessionRow, State};
use crate::view::{edit_text, MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL, WARN};

pub(crate) const RENAME: &str = "session:rename";
pub(crate) const TAG: &str = "session:tag";
pub(crate) const SEARCH: &str = "session:search";

/// Sessions-tab state the rest of the window does not need.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Ui {
    /// The applied search; while the search box is open its buffer filters live instead.
    pub query: String,
    /// A delete waiting for its second click (`sessions.confirm_delete`).
    pub confirm: Option<String>,
    /// A `session-transcribe` running off the UI thread.
    pub transcribing: Option<String>,
}

pub(crate) fn is_ours(path: &str) -> bool {
    path.starts_with("session:")
}

/// The search text in effect: the open search box, else the applied query.
pub fn query(st: &State) -> &str {
    match &st.edit {
        Some(e) if e.path == SEARCH => &e.buf,
        _ => &st.sessions_ui.query,
    }
}

/// Every whitespace-separated word must appear (case-insensitively) in the title, id, start time
/// or a tag; `#word` must match a tag exactly.
pub fn matches(r: &SessionRow, q: &str) -> bool {
    let hay = format!("{} {} {}", r.title, r.id, r.started_at).to_lowercase();
    q.split_whitespace().all(|w| {
        let w = w.to_lowercase();
        match w.strip_prefix('#') {
            Some(t) if !t.is_empty() => r.tags.iter().any(|x| x.to_lowercase() == t),
            _ => hay.contains(&w) || r.tags.iter().any(|x| x.to_lowercase().contains(&w)),
        }
    })
}

pub fn visible(st: &State) -> Vec<&SessionRow> {
    let q = query(st);
    st.sessions.iter().filter(|r| matches(r, q)).collect()
}

/// Enter / save on a `session:*` input. Rename and tag go through their actions; a rejection keeps
/// the input open with the message, as settings edits do.
pub fn commit(st: &mut State, engine: &Engine) {
    let Some(e) = st.edit.clone() else { return };
    let buf = e.buf.trim().to_string();
    if e.path == SEARCH {
        st.sessions_ui.query = buf;
        st.edit = None;
        return;
    }
    let Some(id) = st.selected.clone() else {
        st.edit = None;
        return;
    };
    let res = if e.path == RENAME {
        model::dispatch(
            engine,
            "session-rename",
            json!({"session": id, "name": buf}),
        )
        .map(|_| ())
    } else {
        // Comma-separated adds several tags at once.
        buf.split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .try_for_each(|t| {
                model::dispatch(engine, "session-tag", json!({"session": id, "tag": t})).map(|_| ())
            })
    };
    match res {
        Ok(()) => {
            st.edit = None;
            st.status = String::new();
            st.refresh(engine);
        }
        Err(err) => {
            st.edit = Some(Edit {
                error: Some(err.to_string()),
                ..e
            })
        }
    }
}

pub fn untag(st: &mut State, engine: &Engine, id: &str, tag: &str) {
    st.act(
        engine,
        "session-tag",
        json!({"session": id, "tag": tag, "remove": true}),
    );
}

/// The Delete button. With `sessions.confirm_delete` on, the first press arms it and the second
/// sends `confirm=true`; where it goes (trash or gone) is `sessions.delete_mode`.
pub fn delete(st: &mut State, engine: &Engine, id: &str) {
    let needs = engine.snapshot().config.sessions.confirm_delete;
    if needs && st.sessions_ui.confirm.as_deref() != Some(id) {
        st.sessions_ui.confirm = Some(id.to_string());
        return;
    }
    st.sessions_ui.confirm = None;
    match model::dispatch(
        engine,
        "session-delete",
        json!({"session": id, "confirm": true}),
    ) {
        Ok(v) => {
            if st.selected.as_deref() == Some(id) {
                st.selected = None;
                st.transcript.clear();
                st.edit = None;
            }
            st.status = format!("Deleted {id} ({})", v["mode"].as_str().unwrap_or("?"));
        }
        Err(e) => st.status = e.to_string(),
    }
    st.refresh(engine);
}

/// `session-transcribe` finished: say how it went, reload the list and the shown transcript.
pub fn transcribed(
    st: &mut State,
    engine: &Engine,
    id: &str,
    res: oats_core::config::Result<serde_json::Value>,
) {
    st.sessions_ui.transcribing = None;
    st.status = match res {
        Ok(v) => format!(
            "Transcribed {id} ({} lines)",
            v["summary"]["utterance_count"].as_u64().unwrap_or(0)
        ),
        Err(e) => e.to_string(),
    };
    st.refresh(engine);
    if st.selected.as_deref() == Some(id) {
        let status = std::mem::take(&mut st.status);
        st.select(engine, id);
        if st.status.is_empty() {
            st.status = status;
        }
    }
}

/// The Transcribe button: the ASR runs over the whole recording, so off the UI thread.
fn transcribe(this: &mut MainWindow, id: String, cx: &mut Context<MainWindow>) {
    if this.state.sessions_ui.transcribing.is_some() {
        return;
    }
    this.state.sessions_ui.transcribing = Some(id.clone());
    this.state.status = format!("Transcribing {id}…");
    cx.notify();
    let engine = Arc::clone(&this.engine);
    cx.spawn(async move |view, cx| {
        let (eng, sid) = (Arc::clone(&engine), id.clone());
        let res = cx
            .background_executor()
            .spawn(
                async move { model::dispatch(&eng, "session-transcribe", json!({"session": sid})) },
            )
            .await;
        let _ = view.update(cx, |v, cx| {
            transcribed(&mut v.state, &v.engine, &id, res);
            cx.notify();
        });
    })
    .detach();
}

fn chip(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    colour: u32,
    cx: &mut Context<MainWindow>,
    f: impl Fn(&mut MainWindow, &mut Window, &mut Context<MainWindow>) + 'static,
) -> impl IntoElement {
    let (id, label): (SharedString, SharedString) = (id.into(), label.into());
    let sel = id.to_string();
    div()
        .id(id)
        .debug_selector(|| sel)
        .px_2()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(PANEL))
        .text_color(rgb(colour))
        .text_sm()
        .child(label)
        .on_click(cx.listener(move |this, _, window, cx| f(this, window, cx)))
}

/// Open a `session:*` input seeded with `seed` and give the window the keyboard.
fn open(
    w: &mut MainWindow,
    path: &'static str,
    seed: &str,
    window: &mut Window,
    cx: &mut Context<MainWindow>,
) {
    w.state.begin_edit(path, seed);
    if let Some(f) = &w.focus {
        f.focus(window, cx);
    }
    cx.notify();
}

/// A text box: shows the open buffer with a caret, else `shown` (or `hint` dimmed when empty).
fn input(
    w: &MainWindow,
    path: &'static str,
    shown: &str,
    hint: &str,
    seed: String,
    cx: &mut Context<MainWindow>,
) -> AnyElement {
    let editing = w.state.edit.as_ref().filter(|e| e.path == path);
    let (text, colour) = match editing {
        Some(e) => (edit_text(e).into_any_element(), FG),
        None if shown.is_empty() => (hint.to_string().into_any_element(), DIM),
        None => (shown.to_string().into_any_element(), FG),
    };
    let mut row = div().flex().gap_1().items_center().child(
        div()
            .id(SharedString::from(format!("input-{path}")))
            .flex_1()
            .min_w_0()
            .px_2()
            .rounded_md()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(if editing.is_some() { ACCENT } else { PANEL }))
            .text_color(rgb(colour))
            .cursor_text()
            .child(text)
            .on_click(cx.listener(move |this, _, window, cx| open(this, path, &seed, window, cx))),
    );
    // The search filters as it is typed: its only affordance is the clear chip under it.
    if editing.is_some() && path != SEARCH {
        row = row
            .child(chip(
                format!("{path}-save"),
                "save",
                FG,
                cx,
                |this, _, cx| {
                    commit(&mut this.state, &this.engine);
                    cx.notify();
                },
            ))
            .child(chip(
                format!("{path}-cancel"),
                "cancel",
                FG,
                cx,
                |this, _, cx| {
                    this.state.cancel_edit();
                    cx.notify();
                },
            ));
    }
    let mut col = div().flex().flex_col().gap_1().flex_none().child(row);
    if let Some(err) = editing.and_then(|e| e.error.clone()) {
        col = col.child(div().text_sm().text_color(rgb(ERR)).child(err));
    }
    col.into_any_element()
}

/// The delete confirmation names the session by its title; the id follows as secondary text.
pub(crate) fn confirm_text(del_label: &str, s: &SessionRow) -> String {
    format!("{del_label} {}?", title_of(s))
}

fn title_of(s: &SessionRow) -> String {
    if s.title.is_empty() {
        s.id.clone()
    } else {
        s.title.clone()
    }
}

fn list(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let q = query(&w.state).to_string();
    let rows = visible(&w.state);
    let mut search = div().flex().flex_col().gap_1().flex_none().child(input(
        w,
        SEARCH,
        &q,
        "Search title, id, tags (#tag)",
        q.clone(),
        cx,
    ));
    if !q.is_empty() {
        search = search.child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .text_sm()
                .text_color(rgb(DIM))
                .child(format!("{} of {}", rows.len(), w.state.sessions.len()))
                .child(chip("search-clear", "clear", FG, cx, |this, _, cx| {
                    if this.state.edit.as_ref().is_some_and(|e| e.path == SEARCH) {
                        this.state.cancel_edit();
                    }
                    this.state.sessions_ui.query.clear();
                    cx.notify();
                })),
        );
    }
    let empty = if w.state.sessions.is_empty() {
        "No sessions yet."
    } else {
        "No session matches."
    };
    let items = div()
        .id("sessions")
        .flex()
        .flex_col()
        .gap_1()
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .when(rows.is_empty(), |d| {
            d.child(div().text_color(rgb(DIM)).child(empty))
        })
        .children(rows.into_iter().map(|s| {
            let id = s.id.clone();
            let sel = w.state.selected.as_deref() == Some(s.id.as_str());
            let mut label = format!("{}  ({} lines)", title_of(s), s.utterances);
            if !s.tags.is_empty() {
                label.push_str(&format!("  #{}", s.tags.join(" #")));
            }
            div()
                .id(SharedString::from(format!("session-{}", s.id)))
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .bg(rgb(if sel { ACCENT } else { PANEL }))
                .text_color(rgb(if sel { BG } else { FG }))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this
                        .state
                        .edit
                        .as_ref()
                        .is_some_and(|e| e.path != SEARCH && is_ours(e.path))
                    {
                        this.state.cancel_edit();
                    }
                    this.state.sessions_ui.confirm = None;
                    this.state.select(&this.engine, &id);
                    cx.notify();
                }))
        }));
    div()
        .flex()
        .flex_col()
        .gap_2()
        .w(px(300.))
        .flex_none()
        .child(search)
        .child(items)
        .into_any_element()
}

/// Title, rename, tags and delete for the selected session.
fn manage(w: &MainWindow, s: &SessionRow, cx: &mut Context<MainWindow>) -> AnyElement {
    let cfg = w.engine.snapshot();
    let del_label = match cfg.config.sessions.delete_mode {
        DeleteMode::Trash => "Move to trash",
        DeleteMode::Remove => "Delete",
    };
    let armed = w.state.sessions_ui.confirm.as_deref() == Some(s.id.as_str());
    let (id, id2) = (s.id.clone(), s.id.clone());
    let mut actions = div()
        .flex()
        .gap_2()
        .items_center()
        .child(chip("session-rename", "Rename", FG, cx, {
            let seed = s.title.clone();
            move |this, window, cx| open(this, RENAME, &seed, window, cx)
        }))
        .child(chip(
            "session-tag-add",
            "+ Tag",
            FG,
            cx,
            |this, window, cx| open(this, TAG, "", window, cx),
        ))
        .child(chip(
            "session-delete",
            if armed { "Confirm" } else { del_label },
            if armed { ERR } else { WARN },
            cx,
            move |this, _, cx| {
                delete(&mut this.state, &this.engine, &id);
                cx.notify();
            },
        ));
    if s.utterances == 0 {
        let busy = w.state.sessions_ui.transcribing.as_deref() == Some(s.id.as_str());
        let sid = s.id.clone();
        actions = actions.child(chip(
            "session-transcribe",
            if busy {
                "Transcribing…"
            } else {
                "Transcribe"
            },
            ACCENT,
            cx,
            move |this, _, cx| transcribe(this, sid.clone(), cx),
        ));
    }
    if armed {
        actions = actions
            .child(
                div()
                    .debug_selector(|| "delete-confirm-text".into())
                    .text_sm()
                    .text_color(rgb(ERR))
                    .child(confirm_text(del_label, s)),
            )
            .children((!s.title.is_empty()).then(|| {
                div()
                    .debug_selector(|| "delete-confirm-id".into())
                    .text_sm()
                    .text_color(rgb(DIM))
                    .child(format!("({id2})"))
            }))
            .child(chip(
                "session-delete-cancel",
                "cancel",
                FG,
                cx,
                |this, _, cx| {
                    this.state.sessions_ui.confirm = None;
                    cx.notify();
                },
            ));
    }
    let tags = div()
        .flex()
        .flex_wrap()
        .gap_1()
        .children(s.tags.iter().map(|t| {
            let (sid, tag) = (s.id.clone(), t.clone());
            chip(
                format!("tag-{}-{t}", s.id),
                format!("#{t}  x"),
                ACCENT,
                cx,
                move |this, _, cx| {
                    untag(&mut this.state, &this.engine, &sid, &tag);
                    cx.notify();
                },
            )
        }));
    let editing = w.state.edit.as_ref().map(|e| e.path);
    let mut col = div()
        .flex()
        .flex_col()
        .gap_2()
        .flex_none()
        .child(
            div()
                .flex()
                .gap_3()
                .items_center()
                .child(div().text_color(rgb(ACCENT)).child(title_of(s)))
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(DIM))
                        .child(format!("{}  {}", s.id, s.started_at)),
                ),
        )
        .child(actions)
        .child(tags);
    if editing == Some(RENAME) {
        col = col.child(input(w, RENAME, "", "New title", s.title.clone(), cx));
    }
    if editing == Some(TAG) {
        col = col.child(input(
            w,
            TAG,
            "",
            "tag (comma-separated for several)",
            String::new(),
            cx,
        ));
    }
    col.into_any_element()
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let selected = w
        .state
        .selected
        .as_deref()
        .and_then(|id| w.state.sessions.iter().find(|s| s.id == id))
        .cloned();
    let mut detail = div().flex().flex_col().gap_2().flex_1().min_w_0();
    if let Some(s) = &selected {
        detail = detail.child(manage(w, s, cx));
    }
    detail = detail.child(
        div()
            .id("transcript")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(MainWindow::lines(
                &w.state.transcript,
                if w.state.selected.is_some() {
                    "Empty transcript."
                } else {
                    "Select a session."
                },
                w.speaker_width(),
            )),
    );
    div()
        .flex()
        .gap_4()
        .size_full()
        .child(list(w, cx))
        .child(detail)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    fn seed(dir: &std::path::Path, id: &str, title: &str, tags: &[&str]) {
        let d = dir.join("sessions").join(id);
        std::fs::create_dir_all(&d).unwrap();
        let meta =
            json!({"id": id, "title": title, "startedAt": "2026-10-27T10:00:00Z", "tags": tags});
        std::fs::write(d.join("session.json"), meta.to_string()).unwrap();
        std::fs::write(
            d.join("transcript.final.jsonl"),
            "{\"speaker\":\"you\",\"text\":\"hi\"}\n",
        )
        .unwrap();
    }

    fn row(title: &str, tags: &[&str]) -> SessionRow {
        SessionRow {
            id: "session_1".into(),
            title: title.into(),
            started_at: "2026-10-27T10:00:00Z".into(),
            utterances: 0,
            tags: tags.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn search_matches_every_word_against_title_id_and_tags() {
        let r = row("Weekly Standup", &["work", "team-a"]);
        assert!(matches(&r, ""));
        assert!(matches(&r, "standup"));
        assert!(matches(&r, "WEEKLY session_1"));
        assert!(matches(&r, "team"));
        assert!(matches(&r, "#work"));
        assert!(!matches(&r, "#team"));
        assert!(!matches(&r, "standup retro"));
        assert!(matches(&r, "2026-10-27"));
    }

    fn typed(st: &mut State, path: &'static str, text: &str) {
        st.begin_edit(path, "");
        for c in text.chars() {
            st.edit_key("x", Some(&c.to_string()), false);
        }
    }

    #[test]
    fn rename_tag_untag_search_and_delete_go_through_the_actions() {
        let (engine, dir) = testutil::engine(&[("sessions.delete_mode", json!("remove"))]);
        seed(dir.path(), "a", "Standup", &[]);
        seed(dir.path(), "b", "Retro", &["work"]);
        let mut st = State::default();
        st.refresh(&engine);
        st.select(&engine, "a");

        typed(&mut st, RENAME, "Daily");
        st.commit_edit(&engine);
        assert!(st.edit.is_none());
        let a = |st: &State| st.sessions.iter().find(|s| s.id == "a").cloned().unwrap();
        assert_eq!(a(&st).title, "Daily");

        typed(&mut st, TAG, "work, eng");
        st.commit_edit(&engine);
        assert_eq!(a(&st).tags, vec!["work", "eng"]);
        untag(&mut st, &engine, "a", "work");
        assert_eq!(a(&st).tags, vec!["eng"]);

        // A blank tag input adds nothing and closes.
        typed(&mut st, TAG, " ");
        st.commit_edit(&engine);
        assert!(st.edit.is_none());
        assert_eq!(a(&st).tags, vec!["eng"]);

        // Search filters live while typing and sticks after Enter.
        typed(&mut st, SEARCH, "#work");
        assert_eq!(
            visible(&st)
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        st.commit_edit(&engine);
        assert_eq!(st.sessions_ui.query, "#work");
        assert_eq!(visible(&st).len(), 1);
        st.sessions_ui.query.clear();
        assert_eq!(visible(&st).len(), 2);

        // confirm_delete is on by default: first press arms, second deletes.
        delete(&mut st, &engine, "a");
        assert_eq!(st.sessions.len(), 2);
        assert_eq!(st.sessions_ui.confirm.as_deref(), Some("a"));
        delete(&mut st, &engine, "a");
        assert_eq!(
            st.sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["b"]
        );
        assert!(st.selected.is_none() && st.transcript.is_empty());
        assert!(st.status.starts_with("Deleted a"), "{}", st.status);
        assert!(!dir.path().join("sessions/a").exists());
    }

    struct Says;
    impl oats_core::stt::Asr for Says {
        fn name(&self) -> &'static str {
            "says"
        }
        fn transcribe(
            &self,
            pcm: &[f32],
            _: &str,
        ) -> oats_core::config::Result<Vec<oats_core::stt::Segment>> {
            Ok(vec![oats_core::stt::Segment {
                start: 0.0,
                end: pcm.len() as f64 / 16000.0,
                text: "hello".into(),
            }])
        }
    }

    #[test]
    fn transcribe_fills_a_session_saved_without_a_transcript() {
        let (engine, dir) = testutil::engine(&[]);
        engine.set_asr_factory(Box::new(|_| Ok(Arc::new(Says))));
        let d = dir.path().join("sessions/q");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("session.json"),
            r#"{"id":"q","startedAt":"2026-10-27T10:00:00Z","transcribePending":true}"#,
        )
        .unwrap();
        let mut w = oats_audio::wav::WavWriter::create(&d.join("audio.wav"), 2, 16000).unwrap();
        let tone: Vec<f32> = (0..16000 * 3)
            .flat_map(|i| {
                let v = 0.5 * (i as f32 * 0.05).sin();
                [v, v]
            })
            .collect();
        w.write(&tone).unwrap();
        w.finish().unwrap();
        let mut st = State::default();
        st.refresh(&engine);
        st.select(&engine, "q");
        assert!(st.transcript.is_empty());
        let res = model::dispatch(&engine, "session-transcribe", json!({"session": "q"}));
        st.sessions_ui.transcribing = Some("q".into());
        transcribed(&mut st, &engine, "q", res);
        assert!(st.sessions_ui.transcribing.is_none());
        assert!(st.status.starts_with("Transcribed q"), "{}", st.status);
        assert!(st.sessions.iter().any(|s| s.id == "q" && s.utterances > 0));
        assert!(st.transcript.iter().any(|l| l.text == "hello"));
        // A second press says it already has one.
        let res = model::dispatch(&engine, "session-transcribe", json!({"session": "q"}));
        transcribed(&mut st, &engine, "q", res);
        assert!(
            st.status.contains("already has a transcript"),
            "{}",
            st.status
        );
    }

    #[test]
    fn delete_is_one_press_when_confirm_delete_is_off() {
        let (engine, dir) = testutil::engine(&[
            ("sessions.delete_mode", json!("remove")),
            ("sessions.confirm_delete", json!(false)),
        ]);
        seed(dir.path(), "a", "Standup", &[]);
        let mut st = State::default();
        st.refresh(&engine);
        delete(&mut st, &engine, "a");
        assert!(st.sessions.is_empty());
    }

    #[test]
    fn sessions_view_renders_with_each_input_open_and_delete_armed() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), "a", "Standup", &["work"]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        let steps: [fn(&mut State); 5] = [
            |st| st.selected = None,
            |st| st.begin_edit(SEARCH, "stand"),
            |st| st.begin_edit(RENAME, "x"),
            |st| st.begin_edit(TAG, ""),
            |st| st.sessions_ui.confirm = Some("a".into()),
        ];
        let chips = |cx: &mut gpui::VisualTestContext| {
            [
                cx.debug_bounds("session:search-save").is_some(),
                cx.debug_bounds("session:search-cancel").is_some(),
                cx.debug_bounds("search-clear").is_some(),
            ]
        };
        for step in steps {
            view.update(cx, |v, cx| {
                v.state.tab = Tab::Sessions;
                v.state.select(&v.engine, "a");
                step(&mut v.state);
                cx.notify();
            });
            cx.run_until_parked();
        }
        view.read_with(cx, |v, _| assert_eq!(v.state.sessions.len(), 1));

        // The search box filters live: no save / cancel beside it, only the clear chip.
        view.update(cx, |v, cx| {
            v.state.sessions_ui.confirm = None;
            v.state.begin_edit(SEARCH, "stand");
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(chips(cx), [false, false, true]);
        // Rename keeps its save / cancel; opening it keeps the typed filter.
        view.update(cx, |v, cx| {
            v.state.begin_edit(RENAME, "x");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("session:rename-save").is_some());
        view.read_with(cx, |v, _| assert_eq!(v.state.sessions_ui.query, "stand"));
        view.update(cx, |v, cx| {
            v.state.cancel_edit();
            v.state.sessions_ui.query.clear();
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(chips(cx), [false, false, false]);
    }

    #[test]
    fn delete_confirm_names_the_title_with_the_id_beside_it() {
        let titled = row("Weekly Standup", &[]);
        assert_eq!(
            confirm_text("Move to trash", &titled),
            "Move to trash Weekly Standup?"
        );
        assert_eq!(
            confirm_text("Delete", &row("", &[])),
            "Delete session_1?",
            "untitled sessions fall back to the id"
        );
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), "session_2026-10-29_04-45-46", "Standup", &[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Sessions;
            v.state.select(&v.engine, "session_2026-10-29_04-45-46");
            delete(&mut v.state, &v.engine, "session_2026-10-29_04-45-46");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("delete-confirm-text").is_some());
        assert!(cx.debug_bounds("delete-confirm-id").is_some());
    }

    #[test]
    fn ctrl_a_then_typing_replaces_the_search_through_the_window_keys() {
        let (engine, dir) = testutil::engine(&[]);
        seed(dir.path(), "a", "Standup", &[]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        for (tab, path, seed) in [
            (Tab::Sessions, SEARCH, "stand"),
            (Tab::Settings, "stt.local.model", "base.en"),
        ] {
            view.update(cx, |v, cx| {
                v.state.tab = tab;
                v.state.begin_edit(path, seed);
                cx.notify();
            });
            cx.run_until_parked();
            let focus = view.read_with(cx, |v, _| v.focus.clone().expect("focus"));
            cx.update(|window, cx| focus.focus(window, cx));
            cx.simulate_keystrokes("ctrl-a");
            cx.run_until_parked();
            view.read_with(cx, |v, _| assert!(v.state.edit.as_ref().unwrap().all));
            cx.simulate_keystrokes("r e");
            view.read_with(cx, |v, _| {
                assert_eq!(v.state.edit.as_ref().unwrap().buf, "re", "{path}")
            });
            cx.simulate_keystrokes("ctrl-a backspace");
            view.read_with(cx, |v, _| {
                assert_eq!(v.state.edit.as_ref().unwrap().buf, "", "{path}")
            });
            view.update(cx, |v, _| v.state.cancel_edit());
        }
    }
}
