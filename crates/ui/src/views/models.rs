//! Models tab: every model in the STT catalog from `stt-model-list`, whatever runtime runs it
//! (whisper.cpp or sherpa-onnx), as a table with aligned size, WER and speed (RTFx) columns. The
//! radio picks the default (`config-set stt.local.model`) and, when `stt.local.auto_download` and
//! `network.allow` are both on, downloads it (`stt-model-fetch`); each row's action column offers
//! Use, Download (with progress while it runs) or Delete (`stt-model-delete`), and a failed
//! download shows as an error banner under the table. Nothing is cached here: the list is read on
//! each render, and every `model-download` event re-renders, so progress stays current.

use gpui::{div, prelude::*, px, relative, rgb, AnyElement, Context, FontWeight, SharedString};
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::kit::{self, Kind, Size};
use crate::theme::{self, MONO};
use crate::view::MainWindow;

/// One model as the view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// Catalog id (the `stt.local.model` value).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Download size, e.g. `661.2 MB`.
    pub size: String,
    /// Published word error rate, e.g. `6.0%` (`n/a` when the catalog has none).
    pub wer: String,
    /// How long an hour of audio takes, e.g. `1.1 s` (empty when unpublished). Lower is faster.
    pub speed: String,
    /// `en`, `en/de`, or `99 languages`.
    pub languages: String,
    /// The runtime that runs it: `whisper.cpp`, `sherpa-onnx`.
    pub runtime: String,
    pub status: String,
    /// Download progress 0..=100 while downloading, when the size is known.
    pub percent: Option<u64>,
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

impl ModelRow {
    /// Size, WER, speed, languages and runtime in one line, e.g.
    /// `661.2 MB, WER 6.0%, 1 hr in 1.1 s, en, sherpa-onnx` (the `ui-state` summary).
    pub fn facts(&self) -> String {
        let mut f = vec![self.size.clone(), format!("WER {}", self.wer)];
        if !self.speed.is_empty() {
            f.push(format!("1 hr in {}", self.speed));
        }
        f.push(self.languages.clone());
        f.push(self.runtime.clone());
        f.join(", ")
    }
}

/// How long an hour of audio takes at `rtfx` (seconds of audio per second of compute), in the unit
/// that reads best, so models compare by one number where lower is faster: `1.1 s`, `17 s`, `3 min`.
/// Empty for a speed that is not positive.
fn hour_takes(rtfx: f64) -> String {
    if rtfx <= 0.0 || !rtfx.is_finite() {
        return String::new();
    }
    let secs = 3600.0 / rtfx;
    if secs < 10.0 {
        format!("{secs:.1} s")
    } else if secs < 120.0 {
        format!("{secs:.0} s")
    } else {
        format!("{:.0} min", secs / 60.0)
    }
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.2} GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

fn languages(m: &Value) -> String {
    let langs: Vec<&str> = m["languages"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    match langs.len() {
        0 => "?".into(),
        1..=3 => langs.join("/"),
        n => format!("{n} languages"),
    }
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
            let percent = dl["percent"].as_u64().filter(|_| downloading);
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
                size: mb(m["size"].as_u64().unwrap_or(0)),
                wer: m["wer"]
                    .as_f64()
                    .map_or_else(|| "n/a".into(), |w| format!("{w:.1}%")),
                speed: m["rtfx"]
                    .as_f64()
                    .map(hour_takes)
                    .unwrap_or_default(),
                languages: languages(m),
                runtime: m["runtime"].as_str().unwrap_or("?").to_string(),
                status,
                percent,
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

/// A row's action column: the state badge or button its state calls for, and Delete when the file
/// is there.
fn action(r: &ModelRow, cx: &mut Context<MainWindow>) -> impl IntoElement {
    let t = theme::current();
    let mut col = div()
        .w(px(170.))
        .flex_none()
        .flex()
        .flex_col()
        .items_end()
        .gap(px(4.));
    let badge = |label: String, colour: u32| {
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(colour))
            .child(div().size(px(7.)).rounded_full().bg(rgb(colour)))
            .child(label)
    };
    let mut buttons = div().flex().gap(px(6.)).items_center();
    if r.downloading {
        col = col.child(badge(
            match r.percent {
                Some(p) => format!("Downloading {p}%"),
                None => "Downloading".into(),
            },
            t.accent,
        ));
        col = col.child(
            div()
                .h(px(4.))
                .w(px(150.))
                .rounded(px(2.))
                .bg(rgb(t.line))
                .child(
                    div()
                        .h_full()
                        .rounded(px(2.))
                        .w(relative(r.percent.unwrap_or(0) as f32 / 100.))
                        .bg(rgb(t.accent)),
                ),
        );
        return col;
    }
    if r.selected && r.present {
        buttons = buttons.child(badge("In use".into(), t.ok));
    } else if r.selected {
        buttons = buttons.child(badge("Default".into(), t.accent));
    }
    for (op, on) in [
        (Op::Use, r.can_use && r.present),
        (Op::Download, r.can_download),
        (Op::Delete, r.can_delete && !r.selected),
    ] {
        if !on {
            continue;
        }
        let name = r.id.clone();
        let kind = if op == Op::Delete {
            Kind::Danger
        } else {
            Kind::Secondary
        };
        buttons = buttons.child(
            kit::button(
                format!("model-{}-{}", op.label().to_lowercase(), r.id),
                op.label(),
                kind,
                Size::Sm,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                apply(this, op, &name);
                cx.notify();
            })),
        );
    }
    if !r.compiled {
        buttons = buttons.child(
            div()
                .text_size(px(12.))
                .text_color(rgb(t.faint))
                .child("Not in this build"),
        );
    }
    col.child(buttons)
}

fn num(text: impl Into<SharedString>, w: f32) -> gpui::Div {
    let t = theme::current();
    div()
        .w(px(w))
        .flex_none()
        .text_right()
        .font_family(MONO)
        .text_size(px(12.))
        .text_color(rgb(t.dim))
        .child(text.into())
}

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let t = theme::current();
    let l = load(&w.engine);
    let head = kit::head("Speech models").child(div().flex_1()).child(
        div()
            .text_size(px(12.))
            .text_color(rgb(t.dim))
            .child("Runs on this machine. Audio never leaves it."),
    );
    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(14.))
        .h(px(32.))
        .px(px(16.))
        .bg(rgb(t.raised))
        .border_b_1()
        .border_color(rgb(t.line))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(t.faint))
        .child(div().w(px(22.)).flex_none())
        .child(div().flex_1().child("Model"))
        .child(div().w(px(84.)).flex_none().text_right().child("Size"))
        .child(div().w(px(70.)).flex_none().text_right().child("WER"))
        .child(div().w(px(80.)).flex_none().text_right().child("1 hr takes"))
        .child(div().w(px(170.)).flex_none());
    let mut table = kit::panel().child(header);
    for r in &l.rows {
        let id = r.id.clone();
        // A long name ellipsizes before the Default chip, so the chip never covers Size.
        let mut name = div()
            .flex()
            .items_center()
            .min_w_0()
            .gap(px(6.))
            .font_weight(if r.selected {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .child(kit::ellipsis(r.name.clone()).debug_selector(|| format!("model-name-{}", r.id)));
        if r.selected {
            name = name
                .child(kit::tag("Default", true).debug_selector(|| "model-default-chip".into()));
        }
        let mut row = kit::row(format!("model-{}", r.id), r.selected)
            .h(px(64.))
            .child(div().w(px(22.)).flex_none().child(kit::radio(r.selected)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(name)
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(if r.failed { t.err } else { t.dim }))
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(format!("{} · {} · {}", r.languages, r.runtime, r.status)),
                    ),
            )
            .child(num(r.size.clone(), 84.).debug_selector(|| format!("model-size-{}", r.id)))
            .child(num(r.wer.clone(), 70.))
            .child(num(r.speed.clone(), 80.))
            .child(action(r, cx));
        // The row (its radio) picks the default, as the old dropdown did.
        if r.compiled && !r.selected {
            row = row.on_click(cx.listener(move |this, _, _, cx| {
                select(this, &id);
                cx.notify();
            }));
        }
        table = table.child(row);
    }
    let mut body = div()
        .id("models")
        .flex()
        .flex_col()
        .gap(px(14.))
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px(px(28.))
        .pb(px(22.))
        .child(
            div()
                .flex_none()
                .text_size(px(12.))
                .text_color(rgb(t.dim))
                .child(l.header.clone()),
        );
    if let Some(n) = &l.note {
        body = body.child(kit::banner(false, "A model file is set.", n.clone()));
    }
    body = body.child(table);
    for r in l.rows.iter().filter(|r| r.failed) {
        let id = r.id.clone();
        let why = r.status.trim_start_matches("download failed: ").to_string();
        let mut b = kit::banner(
            true,
            format!("{} failed to download.", r.name),
            format!("{why}. Check the network setting and try again."),
        );
        if r.can_download {
            b = b.child(
                kit::button(
                    format!("model-retry-{}", r.id),
                    "Retry",
                    Kind::Secondary,
                    Size::Sm,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    apply(this, Op::Download, &id);
                    cx.notify();
                })),
            );
        }
        body = body.child(b);
    }
    div()
        .flex()
        .flex_col()
        .size_full()
        .child(head)
        .child(body)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Tab;
    use crate::testutil;
    use gpui::TestAppContext;

    #[test]
    fn hour_takes_reads_in_the_best_unit_and_lower_is_faster() {
        assert_eq!(hour_takes(3386.0), "1.1 s");
        assert_eq!(hour_takes(216.0), "17 s");
        assert_eq!(hour_takes(30.0), "2 min");
        assert_eq!(hour_takes(1.0), "60 min");
        assert_eq!(hour_takes(0.0), "");
        assert_eq!(hour_takes(-5.0), "");
        assert_eq!(hour_takes(f64::NAN), "");
    }

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
                 "size": 661_190_513u64, "wer": 6.02, "rtfx": 3386, "languages": ["en"], "runtime": "sherpa-onnx",
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
        assert_eq!(r[0].facts(), "77.7 MB, WER 12.8%, en, whisper.cpp");
        assert_eq!(
            (r[0].size.as_str(), r[0].wer.as_str()),
            ("77.7 MB", "12.8%")
        );
        assert_eq!(r[1].percent, Some(42));
        assert!(r[0].can_use && r[0].can_delete && !r[0].can_download);
        assert_eq!(r[1].status, "downloading 42%");
        assert!(r[1].selected && !r[1].can_use && !r[1].can_download && !r[1].can_delete);
        assert_eq!(l.selected().unwrap().id, "base.en");
        assert!(r[2].failed && r[2].status.contains("HTTP 404") && r[2].can_download);
        assert_eq!(
            r[3].facts(),
            "661.2 MB, WER 6.0%, 1 hr in 1.1 s, en, sherpa-onnx"
        );
        assert_eq!(
            (r[3].size.as_str(), r[3].wer.as_str(), r[3].speed.as_str()),
            ("661.2 MB", "6.0%", "1.1 s")
        );
        assert_eq!(
            (r[3].languages.as_str(), r[3].runtime.as_str()),
            ("en", "sherpa-onnx")
        );
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
        assert!(l.rows.iter().any(|r| r.runtime == "sherpa-onnx"));
        assert!(l.rows.iter().any(|r| r.runtime == "whisper.cpp"));
        assert_eq!(
            l.selected().map(|r| r.id.as_str()),
            Some("parakeet-tdt-0.6b-v2"),
            "the default model"
        );
        let tiny = l.rows.iter().find(|r| r.id == "tiny.en").unwrap();
        assert!(tiny.can_delete && tiny.status.contains("downloaded"));
        // Picking a row makes it the default.
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
