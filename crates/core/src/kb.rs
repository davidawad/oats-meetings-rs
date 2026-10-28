//! Knowledge base: markdown/text files under `kb.dir` are chunked, embedded and searched by cosine
//! similarity. Ranking is deterministic: scores are rounded to 6 decimals and ties break on
//! (path, chunk index). The default embedder (`hash`) is local and deterministic (signed feature
//! hashing of lowercase word tokens); `ollama` and `openai-compatible` call a model endpoint and go
//! through the same network gate as notes.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::config::typed::{Config, KbEmbedder, LlmBackend};
use crate::config::{ConfigError, Result};
use crate::http;
use crate::notes;
use crate::secrets::{self, SecretStore};

pub const INDEX_FILE: &str = ".oats-kb-index.json";
const INDEX_VERSION: u64 = 1;

fn io(p: &Path, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("io_error", format!("{}: {e}", p.display()))
}

fn fnv(s: &str, seed: u64) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325 ^ seed;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(str::to_string)
        .collect()
}

fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// Local deterministic embedding: sublinear term frequency into hashed, signed buckets.
pub fn hash_embed(text: &str, dims: usize) -> Vec<f32> {
    let mut counts: std::collections::BTreeMap<String, f32> = Default::default();
    for t in tokens(text) {
        *counts.entry(t).or_insert(0.0) += 1.0;
    }
    let mut v = vec![0.0f32; dims];
    for (tok, c) in counts {
        let idx = (fnv(&tok, 0) % dims as u64) as usize;
        let sign = if fnv(&tok, 1) & 1 == 0 { 1.0 } else { -1.0 };
        v[idx] += sign * (1.0 + c.ln());
    }
    normalize(v)
}

/// Paragraph chunks, merged up to `max_chunk` characters (`kb.max_chunk`); fragments under
/// `min_chunk` (`kb.min_chunk`) are dropped.
pub fn chunk(text: &str, max_chunk: usize, min_chunk: usize) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    for para in text.split("\n\n").map(str::trim).filter(|p| !p.is_empty()) {
        if !cur.is_empty() && cur.len() + para.len() + 2 > max_chunk {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push_str("\n\n");
        }
        cur.push_str(para);
        while cur.len() > max_chunk {
            let mut cut = max_chunk;
            while !cur.is_char_boundary(cut) {
                cut -= 1;
            }
            out.push(cur[..cut].to_string());
            cur = cur[cut..].to_string();
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.into_iter().filter(|c| c.chars().count() >= min_chunk).collect()
}

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub dir: PathBuf,
    pub secrets: &'a dyn SecretStore,
}

fn embedder_id(cfg: &Config) -> String {
    match cfg.kb.embedder {
        KbEmbedder::Hash => format!("hash:{}", cfg.kb.hash_dims),
        KbEmbedder::Ollama => format!("ollama:{}", cfg.kb.model),
        KbEmbedder::OpenaiCompatible => format!("openai-compatible:{}", cfg.kb.model),
    }
}

/// Embed `texts` with the configured embedder (gated for remote ones).
fn embed(ctx: &Ctx, texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let cfg = ctx.cfg;
    match cfg.kb.embedder {
        KbEmbedder::Hash => Ok(texts.iter().map(|t| hash_embed(t, cfg.kb.hash_dims as usize)).collect()),
        KbEmbedder::Ollama => {
            let base = if cfg.kb.base_url.is_empty() { notes::OLLAMA_DEFAULT } else { &cfg.kb.base_url };
            let url = http::join(base, "api/embeddings");
            notes::gate(cfg, LlmBackend::Ollama, &url)?;
            texts
                .iter()
                .map(|t| {
                    let r = http::post_json(&url, None, &json!({"model": cfg.kb.model, "prompt": t}), cfg.kb.timeout.as_duration())?;
                    vector_of(&r["embedding"])
                })
                .collect()
        }
        KbEmbedder::OpenaiCompatible => {
            if cfg.kb.base_url.is_empty() {
                return Err(ConfigError::new("base_url_required", "kb.embedder = openai-compatible needs kb.base_url"));
            }
            let url = http::join(&cfg.kb.base_url, "embeddings");
            notes::gate(cfg, LlmBackend::OpenaiCompatible, &url)?;
            let key = match secrets::resolve(ctx.secrets, &cfg.llm.api_key_ref, cfg.secrets.max_key_len as usize) {
                Ok(k) => Some(k),
                Err(e) if e.code == "secret_not_found" => None,
                Err(e) => return Err(e),
            };
            let r = http::post_json(&url, key.as_deref(), &json!({"model": cfg.kb.model, "input": texts}), cfg.kb.timeout.as_duration())?;
            let data = r["data"].as_array().ok_or_else(|| ConfigError::new("backend_bad_response", "no data array"))?;
            if data.len() != texts.len() {
                return Err(ConfigError::new("backend_bad_response", "embedding count mismatch"));
            }
            data.iter().map(|d| vector_of(&d["embedding"])).collect()
        }
    }
}

fn vector_of(v: &Value) -> Result<Vec<f32>> {
    let a = v.as_array().ok_or_else(|| ConfigError::new("backend_bad_response", "embedding is not an array"))?;
    let vec: Option<Vec<f32>> = a.iter().map(|x| x.as_f64().map(|f| f as f32)).collect();
    vec.map(normalize).ok_or_else(|| ConfigError::new("backend_bad_response", "embedding has non-numbers"))
}

/// Source files (relative path, absolute path) in path order.
fn files(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    fn walk(base: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(cur).map_err(|e| io(cur, e))?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let ft = e.file_type().map_err(|e2| io(&p, e2))?;
            if name.starts_with('.') || ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                walk(base, &p, out)?;
            } else if matches!(p.extension().and_then(|x| x.to_str()), Some("md" | "txt")) {
                let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                out.push((rel, p));
            }
        }
        Ok(())
    }
    let mut out = vec![];
    if dir.is_dir() {
        walk(dir, dir, &mut out)?;
    }
    out.sort();
    Ok(out)
}

pub fn reindex(ctx: &Ctx) -> Result<Value> {
    let mut chunks: Vec<(String, usize, String)> = vec![];
    let srcs = files(&ctx.dir)?;
    for (rel, abs) in &srcs {
        let text = std::fs::read_to_string(abs).map_err(|e| io(abs, e))?;
        for (i, c) in chunk(&text, ctx.cfg.kb.max_chunk as usize, ctx.cfg.kb.min_chunk as usize).into_iter().enumerate() {
            chunks.push((rel.clone(), i, c));
        }
    }
    let texts: Vec<String> = chunks.iter().map(|c| c.2.clone()).collect();
    let vectors = embed(ctx, &texts)?;
    let dims = vectors.first().map_or(0, Vec::len);
    let entries: Vec<Value> = chunks
        .iter()
        .zip(&vectors)
        .map(|((path, idx, text), v)| json!({"path": path, "chunk": idx, "text": text, "vector": v}))
        .collect();
    let index = json!({
        "schema_version": INDEX_VERSION,
        "embedder": embedder_id(ctx.cfg),
        "dims": dims,
        "chunks": entries,
    });
    std::fs::create_dir_all(&ctx.dir).map_err(|e| io(&ctx.dir, e))?;
    let path = ctx.dir.join(INDEX_FILE);
    let tmp = ctx.dir.join(format!("{INDEX_FILE}.tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_string(&index).expect("serializable"))
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| io(&path, e))?;
    Ok(json!({"files": srcs.len(), "chunks": chunks.len(), "embedder": embedder_id(ctx.cfg), "index": path}))
}

fn round6(x: f32) -> f64 {
    (f64::from(x) * 1e6).round() / 1e6
}

pub fn search(ctx: &Ctx, query: &str, limit: usize) -> Result<Value> {
    if query.trim().is_empty() {
        return Err(ConfigError::new("invalid_argument", "query must not be empty"));
    }
    let path = ctx.dir.join(INDEX_FILE);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        ConfigError::new("index_missing", format!("no KB index at {}; run kb-reindex", path.display()))
    })?;
    let index: Value = serde_json::from_str(&text)
        .map_err(|e| ConfigError::new("parse_error", format!("{}: {e}", path.display())))?;
    if index["embedder"] != json!(embedder_id(ctx.cfg)) {
        return Err(ConfigError::new(
            "index_stale",
            format!("index was built with {}, config says {}; run kb-reindex", index["embedder"], embedder_id(ctx.cfg)),
        ));
    }
    let q = embed(ctx, &[query.to_string()])?.remove(0);
    let mut scored: Vec<(f64, String, u64, String)> = vec![];
    for c in index["chunks"].as_array().into_iter().flatten() {
        let v = vector_of(&c["vector"])?;
        if v.len() != q.len() {
            return Err(ConfigError::new("index_stale", "index vectors have a different dimension; run kb-reindex"));
        }
        let score = round6(v.iter().zip(&q).map(|(a, b)| a * b).sum());
        scored.push((
            score,
            c["path"].as_str().unwrap_or_default().to_string(),
            c["chunk"].as_u64().unwrap_or(0),
            c["text"].as_str().unwrap_or_default().to_string(),
        ));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(&b.2)));
    let results: Vec<Value> = scored
        .into_iter()
        .take(limit)
        .map(|(score, path, chunk, text)| json!({"path": path, "chunk": chunk, "score": score, "text": text}))
        .collect();
    Ok(json!({"query": query, "count": results.len(), "results": results}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::resolve::{resolve, Inputs};
    use crate::secrets::MemStore;

    fn cfg() -> Config {
        resolve(&Inputs { pack: None, os: "linux", arch: "x", machine: "default", user_toml: None })
            .unwrap()
            .typed()
            .unwrap()
    }

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kb")
    }

    fn built(dir: &Path) -> (Config, MemStore) {
        let cfg = cfg();
        let s = MemStore::default();
        // copy fixture so the index is not written into the repo
        for (rel, abs) in files(&fixture()).unwrap() {
            let to = dir.join(&rel);
            std::fs::create_dir_all(to.parent().unwrap()).unwrap();
            std::fs::copy(abs, to).unwrap();
        }
        (cfg, s)
    }

    fn paths(v: &Value) -> Vec<String> {
        v["results"].as_array().unwrap().iter().map(|r| r["path"].as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn hash_embedding_is_deterministic_unit_length() {
        let d = crate::config::keys::default_int("kb.hash_dims") as usize;
        let a = hash_embed("Annual renewal pricing, annual!", d);
        assert_eq!(a, hash_embed("Annual renewal pricing, annual!", d));
        let n: f32 = a.iter().map(|x| x * x).sum();
        assert!((n - 1.0).abs() < 1e-4, "{n}");
        assert_eq!(hash_embed("   ", d), vec![0.0; d]);
    }

    #[test]
    fn chunking() {
        let (max, min) = (
            crate::config::keys::default_int("kb.max_chunk") as usize,
            crate::config::keys::default_int("kb.min_chunk") as usize,
        );
        assert!(chunk("tiny", max, min).is_empty());
        let big = format!("{}\n\n{}", "a".repeat(1000), "b".repeat(1000));
        let c = chunk(&big, max, min);
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|x| x.len() <= max));
        let long = "word ".repeat(1000);
        assert!(chunk(&long, max, min).iter().all(|x| x.len() <= max));
    }

    /// kb.max_chunk / kb.min_chunk at and around their boundaries.
    /// kb.hash_dims at its bounds: vector length follows it, and the index id records it.
    #[test]
    fn hash_dims_bounds_and_index_id() {
        for dims in [8usize, 256, 8_192] {
            let v = hash_embed("annual renewal pricing", dims);
            assert_eq!(v.len(), dims);
            let n: f32 = v.iter().map(|x| x * x).sum();
            assert!((n - 1.0).abs() < 1e-4, "{dims}: {n}");
        }
    }

    #[test]
    fn chunk_limits_are_honoured() {
        let para = "x".repeat(50);
        let text = format!("{para}\n\n{para}\n\n{para}");
        // Three 50-char paragraphs joined by blank lines are 154 characters.
        assert_eq!(chunk(&text, 154, 1).len(), 1);
        assert_eq!(chunk(&text, 153, 1).len(), 2);
        assert_eq!(chunk(&text, 102, 1).len(), 2);
        assert_eq!(chunk(&text, 101, 1).len(), 3);
        assert_eq!(chunk(&text, 50, 1).len(), 3);
        // A cut piece shorter than min is dropped; exactly min is kept.
        let one = "y".repeat(120);
        assert_eq!(chunk(&one, 100, 20), vec!["y".repeat(100), "y".repeat(20)]);
        assert_eq!(chunk(&one, 100, 21), vec!["y".repeat(100)]);
    }

    #[test]
    fn ranking_is_deterministic_and_sensible_on_the_fixture() {
        let d = tempfile::tempdir().unwrap();
        let (cfg, secrets) = built(d.path());
        let ctx = Ctx { cfg: &cfg, dir: d.path().to_path_buf(), secrets: &secrets };
        let r = reindex(&ctx).unwrap();
        assert_eq!(r["files"], 6);
        for (q, top) in [
            ("annual renewal discount pricing", "pricing.md"),
            ("onboarding checklist for new hires", "onboarding.md"),
            ("security incident response runbook", "security.md"),
        ] {
            let a = search(&ctx, q, 10).unwrap();
            assert_eq!(paths(&a)[0], top, "{q}: {:?}", paths(&a));
            // Same index, same answer, byte for byte; and a rebuilt index gives the same answer.
            assert_eq!(a, search(&ctx, q, 10).unwrap());
            reindex(&ctx).unwrap();
            assert_eq!(a, search(&ctx, q, 10).unwrap(), "{q}");
            // Order is exactly (score desc, path asc, chunk asc).
            let rs = a["results"].as_array().unwrap();
            for w in rs.windows(2) {
                let (s0, s1) = (w[0]["score"].as_f64().unwrap(), w[1]["score"].as_f64().unwrap());
                assert!(s0 >= s1);
                if s0 == s1 {
                    assert!((w[0]["path"].as_str(), w[0]["chunk"].as_u64()) <= (w[1]["path"].as_str(), w[1]["chunk"].as_u64()));
                }
            }
        }
        // Identical documents tie and fall back to path order.
        let a = search(&ctx, "duplicate policy document about travel expenses", 10).unwrap();
        let p = paths(&a);
        let i1 = p.iter().position(|x| x == "dup-a.md").unwrap();
        let i2 = p.iter().position(|x| x == "dup-b.md").unwrap();
        assert_eq!(i2, i1 + 1);
        let rs = a["results"].as_array().unwrap();
        assert_eq!(rs[i1]["score"], rs[i2]["score"]);
        // limit
        assert_eq!(search(&ctx, "pricing", 2).unwrap()["count"], 2);
    }

    #[test]
    fn index_errors() {
        let d = tempfile::tempdir().unwrap();
        let (cfg, secrets) = built(d.path());
        let ctx = Ctx { cfg: &cfg, dir: d.path().to_path_buf(), secrets: &secrets };
        assert_eq!(search(&ctx, "x y", 3).unwrap_err().code, "index_missing");
        reindex(&ctx).unwrap();
        assert_eq!(search(&ctx, "  ", 3).unwrap_err().code, "invalid_argument");
        let mut other = cfg.clone();
        other.kb.model = "m".into();
        other.kb.embedder = KbEmbedder::Ollama;
        let ctx2 = Ctx { cfg: &other, dir: d.path().to_path_buf(), secrets: &secrets };
        assert_eq!(search(&ctx2, "pricing", 3).unwrap_err().code, "index_stale");
        // kb.hash_dims changed: the old index is refused until kb-reindex rebuilds it.
        let mut dims = cfg.clone();
        dims.kb.hash_dims = 16;
        let ctx3 = Ctx { cfg: &dims, dir: d.path().to_path_buf(), secrets: &secrets };
        assert_eq!(search(&ctx3, "pricing", 3).unwrap_err().code, "index_stale");
        reindex(&ctx3).unwrap();
        search(&ctx3, "pricing", 3).unwrap();
    }
}
