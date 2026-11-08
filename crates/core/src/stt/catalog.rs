//! The local STT model catalog (dot-4uzgol.66): every model `stt.local.model` can name, read from
//! `crates/core/data/stt-models.json` (data, not code). An entry says which runtime runs it
//! (whisper.cpp, sherpa-onnx, or CoreML on macOS: dot-4uzgol.69), which files make it up (URL,
//! SHA-256, size), its languages and its published benchmark, so the engine, the model manager and
//! the Models tab all work from it. An entry may name a `fallback` entry, used where this build has
//! no runtime for it or the runtime cannot load it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Value};

const CATALOG: &str = include_str!("../../data/stt-models.json");
/// The speaker-embedding models the diarizer runs (dot-4uzgol.83), same schema, own file: they are
/// not speech models, so `stt.local.model`, `stt-model-list` and the Models tab never list them.
const SPEAKERS: &str = include_str!("../../data/speaker-models.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Runtime {
    #[serde(rename = "whisper.cpp")]
    WhisperCpp,
    #[serde(rename = "sherpa-onnx")]
    SherpaOnnx,
    /// Compiled `.mlmodelc` bundles run by Apple's CoreML (`stt::coreml`), macOS only.
    #[serde(rename = "coreml")]
    CoreMl,
}

impl Runtime {
    pub fn label(self) -> &'static str {
        match self {
            Runtime::WhisperCpp => "whisper.cpp",
            Runtime::SherpaOnnx => "sherpa-onnx",
            Runtime::CoreMl => "coreml",
        }
    }

    /// Whether this build has the runtime (cargo features `whisper` / `parakeet`; CoreML is
    /// built into every macOS build and absent elsewhere).
    pub fn compiled(self) -> bool {
        match self {
            Runtime::WhisperCpp => cfg!(feature = "whisper"),
            Runtime::SherpaOnnx => cfg!(feature = "parakeet"),
            Runtime::CoreMl => cfg!(target_os = "macos"),
        }
    }

    /// How to get a build with this runtime, for error messages.
    pub fn how_to_build(self) -> &'static str {
        match self {
            Runtime::WhisperCpp => "rebuild with --features oats-core/whisper",
            Runtime::SherpaOnnx => "rebuild with --features oats-core/parakeet",
            Runtime::CoreMl => "use the macOS build",
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

/// Token facts a TDT decoder needs (coreml entries).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tdt {
    pub blank: usize,
    /// Encoder frames each duration bin of the joint network advances.
    pub durations: Vec<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub family: String,
    pub runtime: Runtime,
    /// sherpa-onnx model config the files fill (`transducer`, `moonshine`), `tdt` for coreml;
    /// empty for whisper.cpp.
    #[serde(default)]
    pub arch: String,
    /// The entry to use where this build has no runtime for this one, or it cannot load.
    #[serde(default)]
    pub fallback: String,
    #[serde(default)]
    pub tdt: Option<Tdt>,
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

/// Every speaker-embedding entry (crates/core/data/speaker-models.json), in file order.
pub fn speakers() -> &'static [Entry] {
    static C: OnceLock<Vec<Entry>> = OnceLock::new();
    C.get_or_init(|| {
        serde_json::from_str::<Catalog>(SPEAKERS)
            .expect("data/speaker-models.json (checked by the catalog tests)")
            .models
    })
}

pub fn speaker(id: &str) -> Option<&'static Entry> {
    speakers().iter().find(|e| e.id == id)
}

impl Entry {
    /// Total download size.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }

    pub fn file(&self, role: &str) -> Option<&File> {
        self.files.iter().find(|f| f.role == role)
    }

    /// The entry used instead of this one where its runtime is missing or fails to load.
    pub fn fallback(&self) -> Option<&'static Entry> {
        if self.fallback.is_empty() {
            None
        } else {
            get(&self.fallback)
        }
    }

    /// The entry this build runs for this one: itself, or its fallback when this build has no
    /// runtime for it (then a build without the fallback's runtime says that one is missing).
    pub fn resolved(&'static self) -> &'static Entry {
        match self.fallback() {
            Some(f) if !self.runtime.compiled() => f,
            _ => self,
        }
    }

    /// Whether the model is a directory of files (sherpa-onnx, coreml) rather than a single file.
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
            "fallback": (!self.fallback.is_empty()).then_some(&self.fallback),
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
                // A coreml file may sit inside its .mlmodelc bundle: each part is a plain name.
                assert!(f.name.split('/').all(safe_name), "{}: {}", e.id, f.name);
                assert!(
                    e.runtime == Runtime::CoreMl || !f.name.contains('/'),
                    "{}: {}",
                    e.id,
                    f.name
                );
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
                (Runtime::CoreMl, "tdt") => {
                    assert!(safe_name(&e.dir), "{}", e.id);
                    let tdt = e.tdt.as_ref().expect("a tdt entry has its token facts");
                    assert!(!tdt.durations.is_empty());
                    // Each network is one compiled bundle: all its files under one .mlmodelc.
                    for role in ["encoder", "decoder", "joint"] {
                        let files: Vec<&File> = e.files.iter().filter(|f| f.role == role).collect();
                        let bundle = files[0].name.split('/').next().unwrap();
                        assert!(bundle.ends_with(".mlmodelc"), "{}: {role}", e.id);
                        for f in &files {
                            assert!(f.name.starts_with(&format!("{bundle}/")), "{}", f.name);
                            assert!(e.files.iter().filter(|g| g.name == f.name).count() == 1);
                        }
                        for part in ["coremldata.bin", "model.mil", "weights/weight.bin"] {
                            assert!(
                                files.iter().any(|f| f.name == format!("{bundle}/{part}")),
                                "{}: {bundle}/{part}",
                                e.id
                            );
                        }
                    }
                    let mut roles: Vec<&str> = roles.clone();
                    roles.dedup();
                    assert_eq!(roles, ["encoder", "decoder", "joint", "vocab"], "{}", e.id);
                }
                (r, a) => panic!("{}: runtime {r:?} with arch {a:?}", e.id),
            }
            if !e.fallback.is_empty() {
                let f = e.fallback().unwrap_or_else(|| panic!("{}: fallback", e.id));
                assert!(f.fallback.is_empty(), "{}: one fallback hop", e.id);
                assert_eq!(f.languages, e.languages, "{}", e.id);
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
            ("parakeet-tdt-0.6b-v2-coreml", Runtime::CoreMl),
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

    /// dot-4uzgol.69: Parakeet v2 on the Neural Engine is the default; a build without CoreML
    /// (Linux) runs the sherpa-onnx export of the same model instead.
    #[test]
    fn the_coreml_parakeet_falls_back_to_sherpa_where_coreml_is_missing() {
        let e = get("parakeet-tdt-0.6b-v2-coreml").unwrap();
        assert_eq!(
            e.fallback().map(|f| f.id.as_str()),
            Some("parakeet-tdt-0.6b-v2")
        );
        assert_eq!(e.tdt.as_ref().map(|t| t.blank), Some(1024));
        assert_eq!(
            e.file("vocab").map(|f| f.name.as_str()),
            Some("parakeet_vocab.json")
        );
        let r = e.resolved();
        if cfg!(target_os = "macos") {
            assert_eq!(r.id, e.id);
        } else {
            assert_eq!(r.id, "parakeet-tdt-0.6b-v2");
        }
        let w = get("tiny.en").unwrap();
        assert!(std::ptr::eq(w.resolved(), w));
        // A bundle file lives under the model directory at its bundle path.
        let d = tempfile::tempdir().unwrap();
        let p = e.location(d.path());
        for f in &e.files {
            let at = p.join(&f.name);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, b"x").unwrap();
        }
        assert!(e.present(&p));
        std::fs::remove_file(p.join("Encoder.mlmodelc/weights/weight.bin")).unwrap();
        assert!(!e.present(&p));
    }

    /// dot-4uzgol.83: the diarizer's speaker-embedding models, kept apart from the speech models.
    #[test]
    fn the_speaker_models_are_complete_and_the_config_key_allows_the_sherpa_ones() {
        let all = speakers();
        let mut places = std::collections::BTreeSet::new();
        for e in all {
            assert!(safe_name(&e.id), "{}", e.id);
            assert!(get(&e.id).is_none(), "{} is not a speech model too", e.id);
            assert_eq!(e.arch, "speaker-embedding", "{}", e.id);
            assert!(!e.name.is_empty() && !e.files.is_empty(), "{}", e.id);
            for f in &e.files {
                assert!(f.name.split('/').all(safe_name), "{}: {}", e.id, f.name);
                assert!(f.url.starts_with("https://"), "{}: {}", e.id, f.url);
                assert_eq!(f.sha256.len(), 64, "{}: {}", e.id, f.name);
                assert!(f.bytes > 0, "{}: {}", e.id, f.name);
            }
            match e.runtime {
                Runtime::SherpaOnnx => {
                    assert!(!e.is_dir(), "{}: one .onnx file", e.id);
                    assert_eq!(e.files.len(), 1, "{}", e.id);
                }
                Runtime::CoreMl => {
                    assert!(safe_name(&e.dir), "{}", e.id);
                    for role in ["fbank", "embedding"] {
                        assert!(e.files.iter().any(|f| f.role == role), "{}: {role}", e.id);
                    }
                }
                r => panic!("{}: runtime {r:?}", e.id),
            }
            assert!(places.insert(e.location(Path::new("m"))), "{}", e.id);
        }
        let sherpa: Vec<&str> = all
            .iter()
            .filter(|e| e.runtime == Runtime::SherpaOnnx)
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(sherpa, crate::config::keys::STT_SPEAKER_MODELS);
        assert_eq!(
            speaker(crate::stt::diarize::NEURAL_MODEL).map(|e| e.runtime),
            Some(Runtime::CoreMl)
        );
        let crate::config::keys::Def::S(d) =
            crate::config::keys::find("stt.diarization.sherpa_onnx.model")
                .unwrap()
                .default
        else {
            panic!("a string default")
        };
        assert_eq!(speaker(d).map(|e| e.runtime), Some(Runtime::SherpaOnnx));
        // ci/fetch-models.sh fetches the default for tests/diarize_ami.rs, pinned to the catalog.
        let script = include_str!("../../../../ci/fetch-models.sh");
        let ids = script
            .lines()
            .find_map(|l| l.strip_prefix("_speaker_ids="))
            .expect("ci/fetch-models.sh fetches a speaker model");
        assert!(ids.split_whitespace().any(|id| id == d), "{ids}");
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
