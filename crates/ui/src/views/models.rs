//! Models tab: the downloadable whisper models from `stt-model-list`, each with its state (present,
//! size, download progress) and buttons to use it (`config-set stt.local.model`), download it
//! (`stt-model-fetch model=`) or delete it (`stt-model-delete`). Nothing is cached here: the list is
//! read on each render, and every `model-download` event re-renders, so progress stays current.

use gpui::{div, prelude::*, px, rgb, AnyElement, Context, SharedString};
use oats_core::control::engine::Engine;
use serde_json::{json, Value};

use crate::view::{MainWindow, ACCENT, BG, DIM, ERR, FG, PANEL, WARN};

/// One model as the view shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    pub name: String,
    pub status: String,
    pub selected: bool,
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
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Turn an `stt-model-list` reply into rows. Pure, so the button rules are unit-tested.
pub fn listing(v: &Value) -> Listing {
    let engine = v["engine"].as_str().unwrap_or("whisper");
    let explicit = v["explicit_path"].as_str();
    let downloadable = engine == "whisper" && explicit.is_none();
    let note = match (engine, explicit) {
        (_, Some(p)) => Some(format!(
            "stt.local.model_path is set ({p}): that file is used and nothing is downloaded for it."
        )),
        ("whisper", None) => None,
        (e, None) => Some(format!(
            "stt.local.engine is {e}: these whisper models are not used by it (fetch its files with ci/fetch-models.sh)."
        )),
    };
    let rows = v["models"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|m| {
            let present = m["present"].as_bool().unwrap_or(false);
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
            let selected = m["selected"].as_bool().unwrap_or(false);
            ModelRow {
                name: m["name"].as_str().unwrap_or_default().to_string(),
                status,
                selected,
                downloading,
                failed,
                can_use: !selected,
                can_download: downloadable && !present && !downloading,
                can_delete: present && !downloading,
            }
        })
        .collect();
    Listing {
        header: format!(
            "Active model: {}   Models folder: {}",
            v["selected"].as_str().unwrap_or("?"),
            v["models_dir"].as_str().unwrap_or("?")
        ),
        note,
        rows,
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

/// Run a button's action; the result (or error) lands in the status line.
pub fn apply(w: &mut MainWindow, op: Op, model: &str) {
    let (id, params) = op.request(model);
    w.state.act(&w.engine, id, params);
}

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

pub fn render(w: &MainWindow, cx: &mut Context<MainWindow>) -> AnyElement {
    let l = load(&w.engine);
    let mut col = div()
        .id("models")
        .flex()
        .flex_col()
        .gap_2()
        .size_full()
        .overflow_y_scroll()
        .child(div().flex_none().text_color(rgb(DIM)).child(l.header));
    if let Some(n) = l.note {
        col = col.child(div().flex_none().text_color(rgb(WARN)).child(n));
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
            .w(px(140.))
            .flex_none()
            .text_color(rgb(if r.selected { ACCENT } else { FG }))
            .child(if r.selected {
                format!("{} (active)", r.name)
            } else {
                r.name.clone()
            });
        col = col.child(
            div()
                .id(SharedString::from(format!("model-{}", r.name)))
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
                .child(button(&r.name, Op::Use, r.can_use, cx))
                .child(button(&r.name, Op::Download, r.can_download, cx))
                .child(button(&r.name, Op::Delete, r.can_delete, cx)),
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
    fn rows_follow_presence_progress_and_selection() {
        let v = json!({
            "selected": "base.en", "engine": "whisper", "models_dir": "/m", "explicit_path": null,
            "models": [
                {"name": "tiny.en", "present": true, "bytes": 77_700_000u64, "selected": false, "download": null},
                {"name": "base.en", "present": false, "bytes": null, "selected": true,
                 "download": {"state": "downloading", "percent": 42, "bytes": 1}},
                {"name": "small", "present": false, "bytes": null, "selected": false,
                 "download": {"state": "failed", "error": {"code": "x", "message": "HTTP 404"}}},
                {"name": "base", "present": false, "bytes": null, "selected": false, "download": null},
            ]
        });
        let l = listing(&v);
        assert!(l.header.contains("base.en") && l.header.contains("/m"));
        assert_eq!(l.note, None);
        let r = &l.rows;
        assert_eq!(r[0].status, "downloaded, 77.7 MB");
        assert!(r[0].can_use && r[0].can_delete && !r[0].can_download);
        assert_eq!(r[1].status, "downloading 42%");
        assert!(r[1].selected && !r[1].can_use && !r[1].can_download && !r[1].can_delete);
        assert!(r[2].failed && r[2].status.contains("HTTP 404") && r[2].can_download);
        assert_eq!(r[3].status, "not downloaded");
        assert!(r[3].can_download && !r[3].can_delete);
    }

    #[test]
    fn nothing_downloads_for_an_explicit_path_or_another_engine() {
        let m = json!([{"name": "tiny", "present": false, "selected": false, "download": null}]);
        for v in [
            json!({"engine": "whisper", "explicit_path": "/x.bin", "models": m}),
            json!({"engine": "parakeet", "explicit_path": null, "models": m}),
        ] {
            let l = listing(&v);
            assert!(l.note.is_some());
            assert!(!l.rows[0].can_download);
        }
    }

    #[test]
    fn requests_are_well_formed_actions() {
        for op in [Op::Use, Op::Download, Op::Delete] {
            let (id, mut p) = op.request("tiny");
            p["action"] = json!(id);
            serde_json::from_value::<oats_core::action::Action>(p).unwrap();
        }
    }

    #[test]
    fn models_tab_lists_selects_and_deletes_through_the_engine() {
        let (engine, dir) = testutil::engine(&[]);
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join("ggml-tiny.bin"), b"abc").unwrap();
        let mut cx = TestAppContext::single();
        let (view, cx) = cx.add_window_view(|_, _| MainWindow::new(engine.clone()));
        view.update(cx, |v, cx| {
            v.state.tab = Tab::Models;
            cx.notify();
        });
        cx.run_until_parked();
        let l = load(&engine);
        assert_eq!(l.rows.len(), oats_core::stt::models::PINS.len());
        let tiny = l.rows.iter().find(|r| r.name == "tiny").unwrap();
        assert!(tiny.can_delete && tiny.status.contains("downloaded"));
        view.update(cx, |v, cx| {
            apply(v, Op::Use, "tiny");
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            engine.snapshot().config.stt.local.model,
            "tiny",
            "Use sets stt.local.model"
        );
        assert!(load(&engine)
            .rows
            .iter()
            .any(|r| r.name == "tiny" && r.selected));
        view.update(cx, |v, cx| {
            apply(v, Op::Delete, "tiny");
            cx.notify();
        });
        cx.run_until_parked();
        assert!(!models.join("ggml-tiny.bin").exists());
        view.update(cx, |v, cx| {
            apply(v, Op::Delete, "tiny");
            assert!(
                v.state.status.contains("not downloaded"),
                "{}",
                v.state.status
            );
            cx.notify();
        });
        cx.run_until_parked();
    }
}
