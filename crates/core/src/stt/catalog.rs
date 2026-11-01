//! The local STT model catalog (dot-4uzgol.66): every model `stt.local.model` can name, read from
//! `crates/core/data/stt-models.json` (data, not code). An entry says which runtime runs it
//! (whisper.cpp or sherpa-onnx), which files make it up (URL, SHA-256, size), its languages and
//! its published benchmark, so the engine, the model manager and the Models tab all work from it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Value};

const CATALOG: &str = include_str!("../../data/stt-models.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Runtime {
    #[serde(rename = "whisper.cpp")]
    WhisperCpp,
    #[serde(rename = "sherpa-onnx")]
    SherpaOnnx,
}

impl Runtime {
    pub fn label(self) -> &'static str {
        match self {
            Runtime::WhisperCpp => "whisper.cpp",
            Runtime::SherpaOnnx => "sherpa-onnx",
        }
    }

    /// Whether this build has the runtime (cargo features `whisper` / `parakeet`).
    pub fn compiled(self) -> bool {
        match self {
            Runtime::WhisperCpp => cfg!(feature = "whisper"),
            Runtime::SherpaOnnx => cfg!(feature = "parakeet"),
        }
    }

    /// The cargo feature that builds this runtime in, for error messages.
    pub fn feature(self) -> &'static str {
        match self {
            Runtime::WhisperCpp => "whisper",
            Runtime::SherpaOnnx => "parakeet",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    /// What the runtime uses the file as (`model`, `encoder`, `tokens`, ...).
    pub role: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Benchmark {
    /// Average word error rate, percent.
    pub wer: Option<f64>,
    /// Inverse real-time factor (seconds of audio per second of compute).
    pub rtfx: Option<f64>,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub family: String,
    pub runtime: Runtime,
    /// sherpa-onnx model config the files fill (`transducer`, `moonshine`); empty for whisper.cpp.
    #[serde(default)]
    pub arch: String,
    pub languages: Vec<String>,
    /// Sub-directory of the models directory holding the files; empty: one file directly in it.
    pub dir: String,
    pub files: Vec<File>,
    pub benchmark: Benchmark,
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(rename = "_doc")]
    _doc: String,
    models: Vec<Entry>,
}

/// Every catalog entry, in file order.
pub fn all() -> &'static [Entry] {
    static C: OnceLock<Vec<Entry>> = OnceLock::new();
    C.get_or_init(|| {
        serde_json::from_str::<Catalog>(CATALOG)
            .expect("data/stt-models.json (checked by the catalog tests)")
            .models
    })
}

pub fn get(id: &str) -> Option<&'static Entry> {
    all().iter().find(|e| e.id == id)
}

impl Entry {
    /// Total download size.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    pub fn file(&self, role: &str) -> Option<&File> {
        self.files.iter().find(|f| f.role == role)
    }

    /// Whether the model is a directory of files (sherpa-onnx) rather than a single file.
    pub fn is_dir(&self) -> bool {
        !self.dir.is_empty()
    }

    /// Where the model lives under `models_dir`: the file itself, or its directory.
    pub fn location(&self, models_dir: &Path) -> PathBuf {
        if self.is_dir() {
            models_dir.join(&self.dir)
        } else {
            models_dir.join(&self.files[0].name)
        }
    }

    /// Whether the model at `path` (from `location` or `stt.local.model_path`) is all there.
    pub fn present(&self, path: &Path) -> bool {
        if self.is_dir() {
            path.is_dir() && self.files.iter().all(|f| path.join(&f.name).is_file())
        } else {
            path.is_file()
        }
    }

    /// The catalog facts the Models tab and `stt-model-list` show.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "family": self.family, "runtime": self.runtime.label(),
            "languages": self.languages, "size": self.bytes(), "compiled": self.runtime.compiled(),
            "wer": self.benchmark.wer, "rtfx": self.benchmark.rtfx, "benchmark_source": self.benchmark.source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe_name(s: &str) -> bool {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    }

    #[test]
    fn every_entry_is_complete_and_safe() {
        let all = all();
        assert!(!all.is_empty());
        let mut ids = std::collections::BTreeSet::new();
        for e in all {
            assert!(ids.insert(e.id.as_str()), "duplicate id {}", e.id);
            assert!(safe_name(&e.id), "{}", e.id);
            assert!(!e.name.is_empty() && !e.family.is_empty() && !e.languages.is_empty());
            assert!(!e.benchmark.source.is_empty(), "{}: benchmark source", e.id);
            assert!(!e.files.is_empty(), "{}", e.id);
            for f in &e.files {
                assert!(safe_name(&f.name), "{}: {}", e.id, f.name);
                assert!(f.url.starts_with("https://"), "{}: {}", e.id, f.url);
                assert!(
                    f.url.ends_with(&format!("/{}", f.name)),
                    "{}: {}",
                    e.id,
                    f.url
                );
                assert_eq!(f.sha256.len(), 64, "{}: {}", e.id, f.name);
                assert!(f
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
                assert!(f.bytes > 0);
            }
            let roles: Vec<&str> = e.files.iter().map(|f| f.role.as_str()).collect();
            match (e.runtime, e.arch.as_str()) {
                (Runtime::WhisperCpp, "") => {
                    assert!(!e.is_dir(), "{}: a whisper model is one file", e.id);
                    assert_eq!(roles, ["model"], "{}", e.id);
                }
                (Runtime::SherpaOnnx, "transducer") => {
                    assert!(safe_name(&e.dir), "{}", e.id);
                    assert_eq!(
                        roles,
                        ["encoder", "decoder", "joiner", "tokens"],
                        "{}",
                        e.id
                    );
                }
                (Runtime::SherpaOnnx, "moonshine") => {
                    assert!(safe_name(&e.dir), "{}", e.id);
                    assert_eq!(
                        roles,
                        [
                            "preprocessor",
                            "encoder",
                            "uncached_decoder",
                            "cached_decoder",
                            "tokens"
                        ],
                        "{}",
                        e.id
                    );
                }
                (r, a) => panic!("{}: runtime {r:?} with arch {a:?}", e.id),
            }
        }
        // Two models never share a file or directory in the models directory.
        let mut places = std::collections::BTreeSet::new();
        for e in all {
            assert!(places.insert(e.location(Path::new("m"))), "{}", e.id);
        }
    }

    #[test]
    fn the_config_key_allows_exactly_the_catalog_and_defaults_to_it() {
        let ids: Vec<&str> = all().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, crate::config::keys::STT_LOCAL_MODELS);
        let def = crate::config::keys::find("stt.local.model")
            .unwrap()
            .default;
        let crate::config::keys::Def::S(d) = def else {
            panic!("stt.local.model default is a string")
        };
        assert!(get(d).is_some(), "default {d} is in the catalog");
    }

    #[test]
    fn the_bead_seed_models_are_there_for_both_runtimes() {
        for (id, rt) in [
            ("parakeet-tdt-0.6b-v2", Runtime::SherpaOnnx),
            ("large-v3-turbo-q5_0", Runtime::WhisperCpp),
            ("small.en", Runtime::WhisperCpp),
            ("base.en", Runtime::WhisperCpp),
            ("tiny.en", Runtime::WhisperCpp),
            ("moonshine-base-en", Runtime::SherpaOnnx),
        ] {
            assert_eq!(get(id).map(|e| e.runtime), Some(rt), "{id}");
        }
    }

    /// English only (dot-4uzgol.68): every entry but the turbo model David kept for testing is
    /// English, and each dropped multilingual id names a catalog replacement.
    #[test]
    fn the_catalog_is_english_only_and_retired_ids_point_at_it() {
        for e in all() {
            if e.id != "large-v3-turbo-q5_0" {
                assert_eq!(e.languages, ["en"], "{}", e.id);
            }
        }
        for (from, to) in crate::config::keys::STT_RETIRED_MODELS {
            assert!(get(from).is_none(), "{from} is retired");
            assert!(get(to).is_some(), "{from} -> {to}");
        }
    }

    #[test]
    fn presence_needs_every_file_of_a_directory_model() {
        let d = tempfile::tempdir().unwrap();
        let e = get("parakeet-tdt-0.6b-v2").unwrap();
        let p = e.location(d.path());
        assert!(!e.present(&p));
        std::fs::create_dir_all(&p).unwrap();
        for f in &e.files[1..] {
            std::fs::write(p.join(&f.name), b"x").unwrap();
        }
        assert!(!e.present(&p));
        std::fs::write(p.join(&e.files[0].name), b"x").unwrap();
        assert!(e.present(&p));
        let w = get("tiny.en").unwrap();
        assert_eq!(w.location(d.path()), d.path().join("ggml-tiny.en.bin"));
    }
}
