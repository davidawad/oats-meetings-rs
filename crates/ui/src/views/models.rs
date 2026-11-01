//! Models tab (Settings > Models): every model in the STT catalog from `stt-model-list`, whatever
//! runtime runs it (whisper.cpp or sherpa-onnx). At the top, a dropdown of the catalog shows each
//! model's size, published WER, speed (RTFx) and downloaded state; picking one sets the default
//! (`config-set stt.local.model`) and, when `stt.local.auto_download` and `network.allow` are both
//! on, downloads it (`stt-model-fetch`). Below, one row per model with buttons to use it, download
//! it or delete it (`stt-model-delete`). Nothing is cached here: the list is read on each render,
//! and every `model-download` event re-renders, so progress stays current.

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, Global, SharedString};
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::view::{MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL, WARN};

/// One model as the view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// Catalog id (the `stt.local.model` value).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Size, WER, speed and languages, e.g. `661.2 MB, WER 6.0%, RTFx 6000, en, sherpa-onnx`.
    pub facts: String,
    pub status: String,
    pub selected: bool,
    pub present: bool,
    /// This build has the model's runtime.
    pub compiled: bool,
    pub downloading: bool,
    pub failed: bool,
    pub can_use: bool,
    pub can_download: bool,
    pub can_delete: bool,
}

/// What the tab shows: a header line, an optional note, and the rows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub header: String,
    pub note: Option<String>,
    pub rows: Vec<ModelRow>,
    /// Picking a missing model downloads it (`stt.local.auto_download` and `network.allow`).
    pub auto_fetch: bool,
}

impl Listing {
    pub fn selected(&self) -> Option<&ModelRow> {
        self.rows.iter().find(|r| r.selected)
    }
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

fn facts(m: &Value) -> String {
    let mut f = vec![mb(m["size"].as_u64().unwrap_or(0))];
    f.push(match m["wer"].as_f64() {
        Some(w) => format!("WER {w:.1}%"),
        None => "WER n/a".into(),
    });
    if let Some(r) = m["rtfx"].as_f64() {
        f.push(format!("RTFx {r:.0}"));
    }
    let langs: Vec<&str> = m["languages"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    f.push(match langs.len() {
        0 => "?".into(),
        1..=3 => langs.join("/"),
        n => format!("{n} languages"),
    });
    f.push(m["runtime"].as_str().unwrap_or("?").to_string());
    f.join(", ")
}

/// Turn an `stt-model-list` reply into rows. Pure, so the button rules are unit-tested.
pub fn listing(v: &Value) -> Listing {
    let explicit = v["explicit_path"].as_str();
    let downloadable = explicit.is_none();
    let note = explicit.map(|p| {
        format!("stt.local.model_path is set ({p}): that file is used and nothing is downloaded for it.")
    });
    let rows: Vec<ModelRow> = v["models"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let present = m["present"].as_bool().unwrap_or(false);
            let compiled = m["compiled"].as_bool().unwrap_or(true);
            let dl = &m["download"];
            let downloading = dl["state"] == "downloading";
            let failed = !present && dl["state"] == "failed";
            let status = if downloading {
                match dl["percent"].as_u64() {
                    Some(p) => format!("downloading {p}%"),
                    None => format!("downloading {}", mb(dl["bytes"].as_u64().unwrap_or(0))),
                }
            } else if present {
                format!("downloaded, {}", mb(m["bytes"].as_u64().unwrap_or(0)))
            } else if failed {
                format!(
                    "download failed: {}",
                    dl["error"]["message"].as_str().unwrap_or("unknown error")
                )
            } else {
                "not downloaded".to_string()
            };
            let status = if compiled {
                status
            } else {
                format!(
                    "{status}; this build cannot run {}",
                    m["runtime"].as_str().unwrap_or("it")
                )
            };
            let selected = m["selected"].as_bool().unwrap_or(false);
            let id = m["id"].as_str().unwrap_or_default().to_string();
            ModelRow {
                name: m["name"]
                    .as_str()
                    .map_or_else(|| id.clone(), str::to_string),
                id,
                facts: facts(m),
                status,
                selected,
                present,
                compiled,
                downloading,
                failed,
                can_use: !selected && compiled,
                can_download: downloadable && compiled && !present && !downloading,
                can_delete: present && !downloading,
            }
        })
        .collect();
    let active = rows.iter().find(|r| r.selected).map_or_else(
        || v["selected"].as_str().unwrap_or("?").to_string(),
        |r| r.name.clone(),
    );
    Listing {
        header: format!(
            "Active model: {active}   Models folder: {}",
            v["models_dir"].as_str().unwrap_or("?")
        ),
        note,
        rows,
        auto_fetch: downloadable
            && v["auto_download"].as_bool().unwrap_or(false)
            && v["network_allow"].as_bool().unwrap_or(false),
    }
}

/// Read the list from the engine; an error becomes the header so the tab says why it is empty.
pub fn load(engine: &Engine) -> Listing {
    match crate::model::dispatch(engine, "stt-model-list", json!({})) {
        Ok(v) => listing(&v),
        Err(e) => Listing {
            header: e.to_string(),
            ..Listing::default()
        },
    }
}

/// The engine call behind each button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Use,
    Download,
    Delete,
}

impl Op {
    pub fn label(self) -> &'static str {
        match self {
            Op::Use => "Use",
            Op::Download => "Download",
            Op::Delete => "Delete",
        }
    }

    pub fn request(self, model: &str) -> (&'static str, Value) {
        match self {
            Op::Use => (
                "config-set",
                json!({"key": "stt.local.model", "value": model}),
            ),
            Op::Download => ("stt-model-fetch", json!({"model": model})),
            Op::Delete => ("stt-model-delete", json!({"model": model})),
        }
    }
}

/// Run a button's action; the result (or error) lands in the status line. Use is the same as
/// picking the model in the dropdown.
pub fn apply(w: &mut MainWindow, op: Op, model: &str) {
    if op == Op::Use {
        return select(w, model);
    }
    let (id, params) = op.request(model);
    w.state.act(&w.engine, id, params);
}

/// Make `model` the default (`stt.local.model`); when it is missing and `auto_fetch` allows,
/// start downloading it.
pub fn select(w: &mut MainWindow, model: &str) {
    let (id, params) = Op::Use.request(model);
    w.state.act(&w.engine, id, params);
    if !w.state.status.is_empty() {
        return;
    }
    let l = load(&w.engine);
    if l.auto_fetch && l.rows.iter().any(|r| r.id == model && r.can_download) {
        let (id, params) = Op::Download.request(model);
        w.state.act(&w.engine, id, params);
    }
}

/// Whether the model dropdown is open.
#[derive(Default)]
pub struct Picker {
    pub open: bool,
}

impl Global for Picker {}

fn button(model: &str, op: Op, on: bool, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let b = div()
        .id(SharedString::from(format!(
            "model-{}-{}",
            op.label().to_lowercase(),
            model
        )))
        .px_3()
        .rounded_md()
        .child(op.label());
    if !on {
        return b.bg(rgb(PANEL)).text_color(rgb(DIM));
    }
    let name = model.to_string();
    b.cursor_pointer()
        .bg(rgb(ACCENT))
        .text_color(rgb(BG))
        .on_click(cx.listener(move |this, _, _, cx| {
            apply(this, op, &name);
            cx.notify();
        }))
}

fn state_word(r: &ModelRow) -> &str {
    if r.present {
        "downloaded"
    } else if r.downloading {
        "downloading"
    } else {
        "not downloaded"
    }
}

/// The dropdown: a button naming the active model, and when open, one option per catalog model.
fn picker(l: &Listing, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let open = cx.try_global::<Picker>().is_some_and(|p| p.open);
    let current = l.selected().map_or_else(
        || "none".to_string(),
        |r| format!("{} ({}; {})", r.name, r.facts, state_word(r)),
    );
    let mut col = div().flex().flex_col().flex_none().gap_1().child(
        div()
            .flex()
            .gap_2()
            .items_center()
            .child(div().text_color(rgb(DIM)).child("Default model"))
            .child(
                div()
                    .id("models-picker")
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .rounded_md()
                    .cursor_pointer()
                    .bg(rgb(PANEL))
                    .text_color(rgb(FG))
                    .child(format!("{current} {}", if open { "▴" } else { "▾" }))
                    .on_click(cx.listener(|_, _, _, cx| {
                        let p = cx.default_global::<Picker>();
                        p.open = !p.open;
                        cx.notify();
                    })),
            ),
    );
    if open {
        for r in &l.rows {
            let id = r.id.clone();
            let colour = if r.selected {
                ACCENT
            } else if r.compiled {
                FG
            } else {
                DIM
            };
            let mut opt = div()
                .id(SharedString::from(format!("models-pick-{}", r.id)))
                .flex()
                .gap_3()
                .px_2()
                .rounded_md()
                .bg(rgb(PANEL))
                .child(
                    div()
                        .w(px(300.))
                        .flex_none()
                        .text_color(rgb(colour))
                        .child(r.name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(DIM))
                        .child(r.facts.clone()),
                )
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .text_color(rgb(DIM))
                        .child(state_word(r).to_string()),
                );
            if r.compiled {
                opt = opt
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.default_global::<Picker>().open = false;
                        select(this, &id);
                        cx.notify();
                    }));
            }
            col = col.child(opt);
        }
    }
    col
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let l = load(&w.engine);
    let mut col = div()
        .id("models")
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .overflow_y_scroll()
        .child(
            div()
                .flex_none()
                .text_color(rgb(DIM))
                .child(l.header.clone()),
        )
        .child(picker(&l, cx));
    if let Some(n) = &l.note {
        col = col.child(div().flex_none().text_color(rgb(WARN)).child(n.clone()));
    }
    for r in l.rows {
        let colour = if r.failed {
            ERR
        } else if r.downloading {
            WARN
        } else {
            DIM
        };
        let name = div()
            .w(px(300.))
            .flex_none()
            .flex()
            .flex_col()
            .child(
                div()
                    .text_color(rgb(if r.selected { ACCENT } else { FG }))
                    .child(if r.selected {
                        format!("{} (active)", r.name)
                    } else {
                        r.name.clone()
                    }),
            )
            .child(div().text_color(rgb(DIM)).child(r.facts.clone()));
        col = col.child(
            div()
                .id(SharedString::from(format!("model-{}", r.id)))
                .flex()
                .flex_none()
                .gap_3()
                .items_center()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(PANEL))
                .child(name)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(rgb(colour))
                        .child(r.status.clone()),
                )
                .child(button(&r.id, Op::Use, r.can_use, cx))
                .child(button(&r.id, Op::Download, r.can_download, cx))
                .child(button(&r.id, Op::Delete, r.can_delete, cx)),
        );
    }
    col.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    #[test]
    fn rows_follow_presence_progress_selection_and_the_runtime_built_in() {
        let v = json!({
            "selected": "base.en", "runtime": "whisper.cpp", "models_dir": "/m", "explicit_path": null,
            "auto_download": true, "network_allow": false,
            "models": [
                {"id": "tiny.en", "name": "Whisper tiny (English)", "present": true, "bytes": 77_700_000u64,
                 "size": 77_704_715u64, "wer": 12.81, "rtfx": null, "languages": ["en"], "runtime": "whisper.cpp",
                 "compiled": true, "selected": false, "download": null},
                {"id": "base.en", "name": "Whisper base (English)", "present": false, "bytes": null, "compiled": true,
                 "size": 147_964_211u64, "languages": ["en"], "runtime": "whisper.cpp", "selected": true,
                 "download": {"state": "downloading", "percent": 42, "bytes": 1}},
                {"id": "small.en", "name": "Whisper small (English)", "present": false, "bytes": null, "selected": false, "compiled": true,
                 "download": {"state": "failed", "error": {"code": "x", "message": "HTTP 404"}}},
                {"id": "parakeet-tdt-0.6b-v2", "name": "Parakeet TDT 0.6B v2 (English)", "present": false, "bytes": null,
                 "size": 661_190_513u64, "wer": 6.02, "rtfx": 6000, "languages": ["en"], "runtime": "sherpa-onnx",
                 "compiled": true, "selected": false, "download": null},
                {"id": "moonshine-base-en", "name": "Moonshine", "present": false, "runtime": "sherpa-onnx",
                 "compiled": false, "selected": false, "download": null},
            ]
        });
        let l = listing(&v);
        assert!(l.header.contains("Whisper base (English)") && l.header.contains("/m"));
        assert_eq!(l.note, None);
        assert!(!l.auto_fetch, "network.allow is off");
        let r = &l.rows;
        assert_eq!(r[0].status, "downloaded, 77.7 MB");
        assert_eq!(r[0].facts, "77.7 MB, WER 12.8%, en, whisper.cpp");
        assert!(r[0].can_use && r[0].can_delete && !r[0].can_download);
        assert_eq!(r[1].status, "downloading 42%");
        assert!(r[1].selected && !r[1].can_use && !r[1].can_download && !r[1].can_delete);
        assert_eq!(l.selected().unwrap().id, "base.en");
        assert!(r[2].failed && r[2].status.contains("HTTP 404") && r[2].can_download);
        assert_eq!(r[3].facts, "661.2 MB, WER 6.0%, RTFx 6000, en, sherpa-onnx");
        assert!(r[3].can_download && !r[3].can_delete && r[3].can_use);
        assert!(!r[4].can_use && !r[4].can_download);
        assert!(
            r[4].status.contains("cannot run sherpa-onnx"),
            "{}",
            r[4].status
        );
        let mut v2 = v.clone();
        v2["network_allow"] = json!(true);
        assert!(listing(&v2).auto_fetch);
    }

    #[test]
    fn nothing_downloads_for_an_explicit_path() {
        let m = json!([{"id": "tiny.en", "present": false, "selected": false, "compiled": true, "download": null}]);
        let v = json!({"explicit_path": "/x.bin", "auto_download": true, "network_allow": true, "models": m});
        let l = listing(&v);
        assert!(l.note.is_some());
        assert!(!l.rows[0].can_download && !l.auto_fetch);
    }

    #[test]
    fn requests_are_well_formed_actions() {
        for op in [Op::Use, Op::Download, Op::Delete] {
            let (id, mut p) = op.request("tiny.en");
            p["action"] = json!(id);
            serde_json::from_value::<oats_core::action::Action>(p).unwrap();
        }
    }

    #[test]
    fn models_tab_lists_every_runtime_selects_and_deletes_through_the_engine() {
        let (engine, dir) = testutil::engine(&[]);
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("ggml-tiny.en.bin"), b"abc").unwrap();
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Models;
            cx.notify();
        });
        cx.run_until_parked();
        let l = load(&engine);
        assert_eq!(l.rows.len(), oats_core::stt::catalog::all().len());
        assert!(l.rows.iter().any(|r| r.facts.ends_with("sherpa-onnx")));
        assert!(l.rows.iter().any(|r| r.facts.ends_with("whisper.cpp")));
        assert_eq!(
            l.selected().map(|r| r.id.as_str()),
            Some("parakeet-tdt-0.6b-v2"),
            "the default model"
        );
        let tiny = l.rows.iter().find(|r| r.id == "tiny.en").unwrap();
        assert!(tiny.can_delete && tiny.status.contains("downloaded"));
        // The dropdown opens, and picking an entry makes it the default.
        view.update(cx, |_, cx| {
            cx.default_global::<Picker>().open = true;
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            select(v, "tiny.en");
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            engine.snapshot().config.stt.local.model,
            "tiny.en",
            "picking sets stt.local.model"
        );
        assert!(load(&engine)
            .rows
            .iter()
            .any(|r| r.id == "tiny.en" && r.selected));
        // network.allow is off by default: picking a missing model does not download it.
        view.update(cx, |v, cx| {
            select(v, "large-v3-turbo-q5_0");
            assert_eq!(v.state.status, "");
            cx.notify();
        });
        assert_eq!(
            engine.snapshot().config.stt.local.model,
            "large-v3-turbo-q5_0"
        );
        let turbo = load(&engine)
            .rows
            .into_iter()
            .find(|r| r.id == "large-v3-turbo-q5_0")
            .unwrap();
        assert!(!turbo.downloading && !turbo.present);
        view.update(cx, |v, cx| {
            apply(v, Op::Delete, "tiny.en");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(!models.join("ggml-tiny.en.bin").exists());
        view.update(cx, |v, cx| {
            apply(v, Op::Delete, "tiny.en");
            assert!(
                v.state.status.contains("not downloaded"),
                "{}",
                v.state.status
            );
            cx.notify();
        });
        cx.run_until_parked();
    }

    #[test]
    fn picking_a_missing_model_downloads_it_when_auto_download_and_network_allow_are_on() {
        // A mirror that refuses every file: the download starts (and fails) without touching the network.
        let (engine, _dir) = testutil::engine(&[
            ("network.allow", json!(true)),
            ("stt.local.model_url", json!("http://127.0.0.1:9")),
        ]);
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        let rx = engine.bus.subscribe(vec!["model-download".into()]);
        let l = load(&engine);
        assert!(l.auto_fetch);
        // A model this build can run (none when oats-core is built without whisper / parakeet).
        let pick = l
            .rows
            .iter()
            .find(|r| r.compiled && !r.selected)
            .map(|r| r.id.clone());
        let model = pick.clone().unwrap_or_else(|| "base.en".into());
        view.update(cx, |v, cx| {
            select(v, &model);
            cx.notify();
        });
        assert_eq!(engine.snapshot().config.stt.local.model, model);
        if pick.is_some() {
            let ev = rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a model-download event");
            assert!(ev.contains(&model), "{ev}");
        } else {
            assert!(
                rx.try_recv().is_err(),
                "nothing to download into a build that cannot run it"
            );
        }
    }
}
