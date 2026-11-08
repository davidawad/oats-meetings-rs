//! CoreML runtime (macOS, dot-4uzgol.69): NVIDIA Parakeet TDT 0.6B v2 as FluidInference's compiled
//! `.mlmodelc` bundles, on the Apple Neural Engine the way FluidAudio's `AsrManager` runs it,
//! driven from Rust through `objc2-core-ml`.
//!
//! One window (the encoder's fixed input, 15 s): the audio is zero-padded to a whole encoder frame
//! (1280 samples) and to the window, `mel` computes the log-mel features (FluidAudio runs its
//! preprocessor model on the CPU; this is the same computation), the encoder turns them into
//! frames, and `tdt::decode` runs the greedy TDT loop over the decoder (LSTM prediction network)
//! and the joint network (which returns the argmax token, its probability and the duration bin).
//! The encoder, decoder and joint load with `MLComputeUnits.cpuAndNeuralEngine`
//! (`stt.local.provider = cpu`: CPU only). Longer audio is cut into windows in the quiet
//! (`super::pieces`), each decoded from a fresh decoder state.
//!
//! The loaded model is cached per model directory and compute units; config reload clears it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::AnyThread;
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

use super::catalog::Entry;
use super::mel::{Mel, PARAKEET};
use super::tdt::{self, Decision, Limits, Nets};
use super::wav::SAMPLE_RATE;
use super::{Asr, Segment, TdtLimits};
use crate::config::typed::SttProvider;
use crate::config::{ConfigError, Result};

fn fail(what: &str, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("asr_failed", format!("CoreML {what}: {e}"))
}

fn load_failed(what: impl std::fmt::Display) -> ConfigError {
    ConfigError::new("model_load_failed", what.to_string())
}

/// The compute units `stt.local.provider` asks for: the Neural Engine (with the CPU for what it
/// cannot run) unless the setting is `cpu`.
fn units(p: SttProvider) -> (MLComputeUnits, &'static str) {
    match p {
        SttProvider::Cpu => (MLComputeUnits::CPUOnly, "cpuOnly"),
        SttProvider::Auto | SttProvider::Coreml => {
            (MLComputeUnits::CPUAndNeuralEngine, "cpuAndNeuralEngine")
        }
    }
}

/// The compute units the model in `dir` runs on, if it is the one loaded.
pub fn loaded_units(dir: &Path) -> Option<String> {
    CACHE
        .lock()
        .expect("coreml cache")
        .as_ref()
        .filter(|c| c.dir == dir)
        .map(|c| c.units_label.to_string())
}

pub fn clear_cache() {
    *CACHE.lock().expect("coreml cache") = None;
}

struct Loaded {
    encoder: Retained<MLModel>,
    decoder: Retained<MLModel>,
    joint: Retained<MLModel>,
    vocab: Vec<String>,
    blank: usize,
    durations: Vec<usize>,
    /// Encoder input: mel bands and frames (128 x 1501 for 15 s).
    mels: usize,
    mel_frames: usize,
    /// Encoder output size per frame (the joint's `encoder_step`), and samples per encoder frame
    /// (the mel hop times the encoder's subsampling: 1280).
    enc_hidden: usize,
    per_frame: usize,
    /// Prediction network state size per layer, and layers.
    hidden: usize,
    layers: usize,
}

// SAFETY: MLModel is documented thread-safe for prediction; every use goes through `Cached::lock`,
// one decode at a time.
unsafe impl Send for Loaded {}

struct Cached {
    dir: PathBuf,
    units: MLComputeUnits,
    units_label: &'static str,
    loaded: Arc<Mutex<Loaded>>,
}

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

pub struct CoreMlAsr {
    loaded: Arc<Mutex<Loaded>>,
    units_label: &'static str,
    limits: TdtLimits,
    /// Longest stretch decoded at once, in samples (the window, or less by `stt.local.piece`).
    piece: usize,
    mel: Mel,
}

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn shape(dims: &[usize]) -> Retained<NSArray<NSNumber>> {
    let nums: Vec<Retained<NSNumber>> = dims.iter().map(|&d| NSNumber::new_usize(d)).collect();
    NSArray::from_retained_slice(&nums)
}

fn array(dims: &[usize], ty: MLMultiArrayDataType) -> Result<Retained<MLMultiArray>> {
    unsafe { MLMultiArray::initWithShape_dataType_error(MLMultiArray::alloc(), &shape(dims), ty) }
        .map_err(|e| fail("array", e.localizedDescription()))
}

fn dims_of(a: &MLMultiArray) -> (Vec<usize>, Vec<usize>) {
    let get = |v: Retained<NSArray<NSNumber>>| -> Vec<usize> {
        v.to_vec().iter().map(|n| n.as_usize()).collect()
    };
    unsafe { (get(a.shape()), get(a.strides())) }
}

/// Every element of `a` in row-major order, as f32 (any float or int32 array).
fn read(a: &MLMultiArray) -> Result<Vec<f32>> {
    let (dims, strides) = dims_of(a);
    let n: usize = dims.iter().product();
    let ty = unsafe { a.dataType() };
    #[allow(deprecated)]
    let base = unsafe { a.dataPointer() }.as_ptr();
    let mut out = Vec::with_capacity(n);
    let mut idx = vec![0usize; dims.len()];
    for _ in 0..n {
        let off: usize = idx.iter().zip(&strides).map(|(i, s)| i * s).sum();
        // SAFETY: `off` is inside the array by its shape and strides; the type decides the width.
        let v = unsafe {
            if ty == MLMultiArrayDataType::Float32 {
                *(base as *const f32).add(off)
            } else if ty == MLMultiArrayDataType::Float16 {
                f16_to_f32(*(base as *const u16).add(off))
            } else if ty == MLMultiArrayDataType::Double {
                *(base as *const f64).add(off) as f32
            } else if ty == MLMultiArrayDataType::Int32 {
                *(base as *const i32).add(off) as f32
            } else {
                return Err(fail("output", format!("unsupported data type {}", ty.0)));
            }
        };
        out.push(v);
        for d in (0..dims.len()).rev() {
            idx[d] += 1;
            if idx[d] < dims[d] {
                break;
            }
            idx[d] = 0;
        }
    }
    Ok(out)
}

/// Write `data` (row-major) into a float32 array `a` through its strides.
fn write_f32(a: &MLMultiArray, data: &[f32]) {
    let (dims, strides) = dims_of(a);
    #[allow(deprecated)]
    let base = unsafe { a.dataPointer() }.as_ptr() as *mut f32;
    let mut idx = vec![0usize; dims.len()];
    for &v in data.iter().take(dims.iter().product()) {
        let off: usize = idx.iter().zip(&strides).map(|(i, s)| i * s).sum();
        // SAFETY: `a` is a float32 array and `off` lies inside it.
        unsafe { *base.add(off) = v };
        for d in (0..dims.len()).rev() {
            idx[d] += 1;
            if idx[d] < dims[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn write_i32(a: &MLMultiArray, v: i32) {
    #[allow(deprecated)]
    let base = unsafe { a.dataPointer() }.as_ptr() as *mut i32;
    // SAFETY: `a` is an int32 array of at least one element.
    unsafe { *base = v };
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = u32::from(h >> 15) << 31;
    let exp = u32::from((h >> 10) & 0x1f);
    let frac = u32::from(h & 0x3ff);
    let bits = match (exp, frac) {
        (0, 0) => sign,
        (0, f) => {
            // Subnormal: renormalise.
            let shift = f.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((f << shift) & 0x3ff) << 13)
        }
        (0x1f, f) => sign | 0x7f80_0000 | (f << 13),
        (e, f) => sign | ((e + 112) << 23) | (f << 13),
    };
    f32::from_bits(bits)
}

/// A feature provider over `inputs`; the arrays stay referenced, so writing into them changes
/// the next prediction's input.
fn provider(inputs: &[(&str, &MLMultiArray)]) -> Result<Retained<MLDictionaryFeatureProvider>> {
    let keys: Vec<Retained<NSString>> = inputs.iter().map(|(k, _)| ns(k)).collect();
    let vals: Vec<Retained<MLFeatureValue>> = inputs
        .iter()
        .map(|(_, a)| unsafe { MLFeatureValue::featureValueWithMultiArray(a) })
        .collect();
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let val_refs: Vec<&AnyObject> = vals.iter().map(|v| -> &AnyObject { v }).collect();
    let dict: Retained<NSDictionary<NSString, AnyObject>> =
        NSDictionary::from_slices(&key_refs, &val_refs);
    unsafe {
        MLDictionaryFeatureProvider::initWithDictionary_error(
            MLDictionaryFeatureProvider::alloc(),
            &dict,
        )
    }
    .map_err(|e| fail("input", e.localizedDescription()))
}

fn predict(
    model: &MLModel,
    input: &MLDictionaryFeatureProvider,
    what: &str,
) -> Result<Retained<ProtocolObject<dyn MLFeatureProvider>>> {
    unsafe { model.predictionFromFeatures_error(ProtocolObject::from_ref(input)) }
        .map_err(|e| fail(what, e.localizedDescription()))
}

fn output(
    out: &ProtocolObject<dyn MLFeatureProvider>,
    name: &str,
) -> Result<Retained<MLMultiArray>> {
    unsafe { out.featureValueForName(&ns(name)) }
        .and_then(|v| unsafe { v.multiArrayValue() })
        .ok_or_else(|| fail("output", format!("no {name}")))
}

/// The fixed shape of input (or output) `name` of `model`.
fn io_shape(model: &MLModel, name: &str, input: bool) -> Result<Vec<usize>> {
    let md = unsafe { model.modelDescription() };
    let desc = unsafe {
        if input {
            md.inputDescriptionsByName()
        } else {
            md.outputDescriptionsByName()
        }
    };
    let d = desc
        .objectForKey(&ns(name))
        .ok_or_else(|| load_failed(format!("model has no {name}")))?;
    let c = unsafe { d.multiArrayConstraint() }
        .ok_or_else(|| load_failed(format!("{name} is not an array")))?;
    Ok(unsafe { c.shape() }
        .to_vec()
        .iter()
        .map(|n| n.as_usize())
        .collect())
}

fn load_model(path: &Path, units: MLComputeUnits) -> Result<Retained<MLModel>> {
    let url = NSURL::fileURLWithPath(&ns(&path.display().to_string()));
    let cfg = unsafe { MLModelConfiguration::new() };
    unsafe { cfg.setComputeUnits(units) };
    unsafe { MLModel::modelWithContentsOfURL_configuration_error(&url, &cfg) }.map_err(|e| {
        load_failed(format!(
            "CoreML could not load {}: {}",
            path.display(),
            e.localizedDescription()
        ))
    })
}

/// `parakeet_vocab.json`: token id (as a string) -> SentencePiece piece.
fn vocab(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| load_failed(format!("{}: {e}", path.display())))?;
    let map: std::collections::BTreeMap<String, String> =
        serde_json::from_str(&text).map_err(|e| load_failed(format!("{}: {e}", path.display())))?;
    let mut v = Vec::new();
    for (id, piece) in map {
        let id: usize = id
            .parse()
            .map_err(|_| load_failed(format!("{}: token id {id:?}", path.display())))?;
        if v.len() <= id {
            v.resize(id + 1, String::new());
        }
        v[id] = piece;
    }
    Ok(v)
}

/// The bundle directory (`X.mlmodelc`) that `role`'s files sit in.
fn bundle(entry: &Entry, dir: &Path, role: &str) -> Result<PathBuf> {
    let f = entry
        .file(role)
        .ok_or_else(|| load_failed(format!("catalog entry {} has no {role}", entry.id)))?;
    Ok(dir.join(f.name.split('/').next().unwrap_or(&f.name)))
}

impl CoreMlAsr {
    /// Load `entry`'s bundles from `dir` on the compute units `provider` asks for.
    pub fn load(
        entry: &Entry,
        dir: &Path,
        provider: SttProvider,
        limits: TdtLimits,
    ) -> Result<CoreMlAsr> {
        let (units, units_label) = units(provider);
        let mut cache = CACHE.lock().expect("coreml cache");
        if let Some(c) = cache.as_ref() {
            if c.dir == dir && c.units == units {
                return Ok(CoreMlAsr::with(Arc::clone(&c.loaded), units_label, limits));
            }
        }
        let tdt = entry
            .tdt
            .as_ref()
            .ok_or_else(|| load_failed(format!("catalog entry {} has no tdt facts", entry.id)))?;
        let loaded = autoreleasepool(|_| -> Result<Loaded> {
            let encoder = load_model(&bundle(entry, dir, "encoder")?, units)?;
            let decoder = load_model(&bundle(entry, dir, "decoder")?, units)?;
            let joint = load_model(&bundle(entry, dir, "joint")?, units)?;
            let vocab_file = entry
                .file("vocab")
                .ok_or_else(|| load_failed(format!("catalog entry {} has no vocab", entry.id)))?;
            let vocab = vocab(&dir.join(&vocab_file.name))?;
            let mel = io_shape(&encoder, "mel", true)?;
            let enc = io_shape(&encoder, "encoder", false)?;
            let state = io_shape(&decoder, "h_in", true)?;
            let step = io_shape(&joint, "encoder_step", true)?;
            match (mel.as_slice(), enc.as_slice(), state.as_slice(), step.as_slice()) {
                ([1, mels, frames], [1, eh, ef], [layers, 1, hidden], [1, sh, 1])
                    if *mels == PARAKEET.mels && eh == sh && *ef > 0 && *frames > 1 =>
                {
                    let sub = ((frames - 1) as f64 / *ef as f64).round().max(1.0) as usize;
                    Ok(Loaded {
                    mels: *mels,
                    mel_frames: *frames,
                    enc_hidden: *sh,
                    per_frame: PARAKEET.hop * sub,
                    layers: *layers,
                    hidden: *hidden,
                    encoder,
                    decoder,
                    joint,
                    vocab,
                    blank: tdt.blank,
                    durations: tdt.durations.clone(),
                })
                }
                _ => Err(load_failed(format!(
                    "{}: unexpected shapes: encoder {mel:?} -> {enc:?}, decoder state {state:?}, joint {step:?}",
                    dir.display()
                ))),
            }
        })?;
        super::idle::loaded(&format!("{} on {units_label}", entry.id));
        let loaded = Arc::new(Mutex::new(loaded));
        *cache = Some(Cached {
            dir: dir.to_path_buf(),
            units,
            units_label,
            loaded: Arc::clone(&loaded),
        });
        Ok(CoreMlAsr::with(loaded, units_label, limits))
    }

    fn with(loaded: Arc<Mutex<Loaded>>, units_label: &'static str, limits: TdtLimits) -> CoreMlAsr {
        CoreMlAsr {
            loaded,
            units_label,
            limits,
            piece: usize::MAX,
            mel: Mel::new(PARAKEET),
        }
    }

    /// Decode at most `samples` at once (never more than the encoder window).
    pub fn with_piece(mut self, samples: usize) -> CoreMlAsr {
        self.piece = samples.max(1);
        self
    }

    /// Samples one encoder window holds (240 000 for 1501 mel frames).
    fn window(l: &Loaded) -> usize {
        (l.mel_frames - 1) * PARAKEET.hop
    }

    fn decode_window(&self, l: &Loaded, pcm: &[f32]) -> Result<Vec<Segment>> {
        autoreleasepool(|_| self.decode_window_inner(l, pcm))
    }

    fn decode_window_inner(&self, l: &Loaded, pcm: &[f32]) -> Result<Vec<Segment>> {
        let window = Self::window(l);
        let pcm = &pcm[..pcm.len().min(window)];
        // FluidAudio pads the audio to a whole encoder frame (when that still fits the window)
        // and declares that length to the preprocessor.
        let aligned = (pcm.len().div_ceil(l.per_frame) * l.per_frame).min(window);
        let mut padded = pcm.to_vec();
        padded.resize(aligned.max(pcm.len()), 0.0);
        let (mel, mel_len) = self.mel.features(&padded, l.mel_frames);
        let mel_in = array(&[1, l.mels, l.mel_frames], MLMultiArrayDataType::Float32)?;
        write_f32(&mel_in, &mel);
        let len_in = array(&[1], MLMultiArrayDataType::Int32)?;
        write_i32(&len_in, i32::try_from(mel_len).unwrap_or(i32::MAX));
        let out = predict(
            &l.encoder,
            &*provider(&[("mel", &mel_in), ("mel_length", &len_in)])?,
            "encoder",
        )?;
        let enc = output(&out, "encoder")?;
        let (dims, _) = dims_of(&enc);
        let hidden = l.enc_hidden;
        // The export is [1, hidden, frames]; take [1, frames, hidden] too.
        let (frames, channels_first) = match dims.as_slice() {
            [1, h, f] if *h == hidden => (*f, true),
            [1, f, h] if *h == hidden => (*f, false),
            _ => return Err(fail("encoder", format!("output shape {dims:?}"))),
        };
        let raw = read(&enc)?;
        // Frame-major: frame t is enc_frames[t * hidden..][..hidden].
        let mut enc_frames = vec![0.0f32; hidden * frames];
        for t in 0..frames {
            for h in 0..hidden {
                enc_frames[t * hidden + h] = if channels_first {
                    raw[h * frames + t]
                } else {
                    raw[t * hidden + h]
                };
            }
        }
        let enc_len = read(&*output(&out, "encoder_length")?)?
            .first()
            .map_or(frames, |&v| (v.max(0.0) as usize).min(frames));
        let audio_frames = padded.len().div_ceil(l.per_frame);
        let mut nets = CoreMlNets::new(l, enc_frames)?;
        let hyp = tdt::decode(
            &mut nets,
            enc_len,
            audio_frames,
            &Limits {
                blank: l.blank,
                durations: &l.durations,
                max_symbols: self.limits.max_symbols,
                max_tokens: self.limits.max_tokens,
                tail_blanks: self.limits.tail_blanks,
            },
        )?;
        let secs = l.per_frame as f32 / SAMPLE_RATE as f32;
        let (mut tokens, mut times, mut durs) = (vec![], vec![], vec![]);
        for i in 0..hyp.tokens.len() {
            let piece = l.vocab.get(hyp.tokens[i]).map_or("", String::as_str);
            // FluidAudio drops <unk> (and anything outside the vocabulary).
            if piece.is_empty() || piece == "<unk>" {
                continue;
            }
            tokens.push(piece.to_string());
            times.push(hyp.frames[i] as f32 * secs);
            durs.push(hyp.durations[i] as f32 * secs);
        }
        let end = pcm.len() as f64 / f64::from(SAMPLE_RATE);
        Ok(super::sentences(&tokens, &times, Some(&durs), end))
    }
}

/// The decoder and joint networks over one window's encoder frames.
struct CoreMlNets<'a> {
    l: &'a Loaded,
    enc: Vec<f32>,
    targets: Retained<MLMultiArray>,
    h: Retained<MLMultiArray>,
    c: Retained<MLMultiArray>,
    dec_in: Retained<MLDictionaryFeatureProvider>,
    enc_step: Retained<MLMultiArray>,
    dec_step: Retained<MLMultiArray>,
    joint_in: Retained<MLDictionaryFeatureProvider>,
}

impl<'a> CoreMlNets<'a> {
    fn new(l: &'a Loaded, enc: Vec<f32>) -> Result<CoreMlNets<'a>> {
        let f32t = MLMultiArrayDataType::Float32;
        let targets = array(&[1, 1], MLMultiArrayDataType::Int32)?;
        let target_length = array(&[1], MLMultiArrayDataType::Int32)?;
        write_i32(&target_length, 1);
        let h = array(&[l.layers, 1, l.hidden], f32t)?;
        let c = array(&[l.layers, 1, l.hidden], f32t)?;
        let zeros = vec![0.0f32; l.layers * l.hidden];
        write_f32(&h, &zeros);
        write_f32(&c, &zeros);
        let dec_in = provider(&[
            ("targets", &targets),
            ("target_length", &target_length),
            ("h_in", &h),
            ("c_in", &c),
        ])?;
        let enc_step = array(&[1, l.enc_hidden, 1], f32t)?;
        let dec_step = array(&[1, l.hidden, 1], f32t)?;
        let joint_in = provider(&[("encoder_step", &enc_step), ("decoder_step", &dec_step)])?;
        Ok(CoreMlNets {
            l,
            enc,
            targets,
            h,
            c,
            dec_in,
            enc_step,
            dec_step,
            joint_in,
        })
    }
}

impl Nets for CoreMlNets<'_> {
    fn predict(&mut self, token: usize) -> Result<()> {
        write_i32(&self.targets, i32::try_from(token).unwrap_or(0));
        let out = predict(&self.l.decoder, &self.dec_in, "decoder")?;
        write_f32(&self.h, &read(&*output(&out, "h_out")?)?);
        write_f32(&self.c, &read(&*output(&out, "c_out")?)?);
        write_f32(&self.dec_step, &read(&*output(&out, "decoder")?)?);
        Ok(())
    }

    fn joint(&mut self, frame: usize) -> Result<Decision> {
        let n = self.l.enc_hidden;
        let at = frame * n;
        let step = self
            .enc
            .get(at..at + n)
            .ok_or_else(|| fail("joint", format!("frame {frame}")))?;
        write_f32(&self.enc_step, step);
        let out = predict(&self.l.joint, &self.joint_in, "joint")?;
        let first = |name: &str| -> Result<f32> {
            read(&*output(&out, name)?)?
                .first()
                .copied()
                .ok_or_else(|| fail("joint", format!("empty {name}")))
        };
        Ok(Decision {
            token: first("token_id")?.max(0.0) as usize,
            prob: first("token_prob")?,
            duration_bin: first("duration")?.max(0.0) as usize,
        })
    }
}

impl Asr for CoreMlAsr {
    fn name(&self) -> &'static str {
        "coreml"
    }

    fn provider(&self) -> &str {
        self.units_label
    }

    /// Parakeet v2 is English-only: `language` is accepted and ignored. Audio longer than the
    /// encoder window (or `stt.local.piece`) is decoded window by window, cut in the quiet; each
    /// window becomes one segment per sentence.
    fn transcribe(&self, pcm: &[f32], _language: &str) -> Result<Vec<Segment>> {
        let l = self.loaded.lock().expect("coreml model");
        let max = self.piece.min(Self::window(&l));
        let mut out = Vec::new();
        for (at, piece) in super::pieces(pcm, max) {
            let off = at as f64 / f64::from(SAMPLE_RATE);
            out.extend(self.decode_window(&l, piece)?.into_iter().map(|s| Segment {
                start: s.start + off,
                end: s.end + off,
                text: s.text,
            }));
        }
        Ok(out)
    }
}

/// The pyannote community-1 speaker-embedding networks as FluidAudio runs them
/// (`stt.diarization.algorithm = neural`, dot-4uzgol.83): `FBank.mlmodelc` turns a 10 s window of
/// audio into 80 x 998 filterbank features (it scales the samples to 16-bit range itself), and
/// `Embedding.mlmodelc` (WeSpeaker ResNet34) pools them, weighted per segmentation frame (589 per
/// window), into a 256-d speaker embedding. An utterance shorter than the window is zero-padded
/// with the padding weighted out; a longer one is embedded window by window and the embeddings
/// averaged by their audio.
pub struct SpeakerNet {
    fbank: Retained<MLModel>,
    embedding: Retained<MLModel>,
    units_label: &'static str,
    /// Samples per window (160 000) and weight frames per window (589).
    window: usize,
    frames: usize,
}

// SAFETY: as for `Loaded`: MLModel is thread-safe for prediction and every use goes through the
// cache's mutex, one utterance at a time.
unsafe impl Send for SpeakerNet {}

type SpeakerCached = (PathBuf, MLComputeUnits, Arc<Mutex<SpeakerNet>>);

static SPEAKER_CACHE: Mutex<Option<SpeakerCached>> = Mutex::new(None);

pub fn clear_speaker_cache() {
    *SPEAKER_CACHE.lock().expect("coreml speaker cache") = None;
}

/// Whether a loaded recogniser or speaker model is held outside its cache (`idle`).
pub fn in_use() -> bool {
    let asr = CACHE
        .lock()
        .expect("coreml cache")
        .as_ref()
        .is_some_and(|c| Arc::strong_count(&c.loaded) > 1);
    let speaker = SPEAKER_CACHE
        .lock()
        .expect("coreml speaker cache")
        .as_ref()
        .is_some_and(|c| Arc::strong_count(&c.2) > 1);
    asr || speaker
}

impl SpeakerNet {
    /// Load `entry`'s `fbank` and `embedding` bundles from `dir` on the compute units `provider`
    /// asks for. Cached per directory and compute units.
    pub fn load(
        entry: &Entry,
        dir: &Path,
        provider: SttProvider,
    ) -> Result<Arc<Mutex<SpeakerNet>>> {
        let (units, units_label) = units(provider);
        let mut cache = SPEAKER_CACHE.lock().expect("coreml speaker cache");
        if let Some((d, u, l)) = cache.as_ref() {
            if d == dir && *u == units {
                return Ok(Arc::clone(l));
            }
        }
        let net = autoreleasepool(|_| -> Result<SpeakerNet> {
            let fbank = load_model(&bundle(entry, dir, "fbank")?, units)?;
            let embedding = load_model(&bundle(entry, dir, "embedding")?, units)?;
            let audio = io_shape(&fbank, "audio", true)?;
            let weights = io_shape(&embedding, "weights", true)?;
            match (audio.as_slice(), weights.as_slice()) {
                ([1, 1, window], [1, frames]) if *window > 0 && *frames > 0 => Ok(SpeakerNet {
                    fbank,
                    embedding,
                    units_label,
                    window: *window,
                    frames: *frames,
                }),
                _ => Err(load_failed(format!(
                    "{}: unexpected shapes: fbank audio {audio:?}, embedding weights {weights:?}",
                    dir.display()
                ))),
            }
        })?;
        super::idle::loaded(&format!("speaker model {} on {units_label}", entry.id));
        let net = Arc::new(Mutex::new(net));
        *cache = Some((dir.to_path_buf(), units, Arc::clone(&net)));
        Ok(net)
    }

    pub fn units_label(&self) -> &'static str {
        self.units_label
    }

    /// The embedding of `pcm` (16 kHz mono), averaged over its windows by their audio.
    pub fn embed(&self, pcm: &[f32]) -> Result<Option<Vec<f32>>> {
        let mut sum: Vec<f32> = Vec::new();
        for chunk in pcm.chunks(self.window) {
            let e = autoreleasepool(|_| self.embed_window(chunk))?;
            if sum.is_empty() {
                sum = vec![0.0; e.len()];
            }
            let w = chunk.len() as f32 / self.window as f32;
            sum.iter_mut().zip(&e).for_each(|(s, x)| *s += w * x);
        }
        Ok((!sum.is_empty() && sum.iter().all(|x| x.is_finite())).then_some(sum))
    }

    fn embed_window(&self, chunk: &[f32]) -> Result<Vec<f32>> {
        let f32t = MLMultiArrayDataType::Float32;
        let audio = array(&[1, 1, self.window], f32t)?;
        let mut padded = chunk.to_vec();
        padded.resize(self.window, 0.0);
        write_f32(&audio, &padded);
        let out = predict(&self.fbank, &*provider(&[("audio", &audio)])?, "fbank")?;
        let features = output(&out, "fbank_features")?;
        // The weight frames the audio covers; the zero padding after it is weighted out.
        let live = (chunk.len() * self.frames)
            .div_ceil(self.window)
            .clamp(1, self.frames);
        let weights = array(&[1, self.frames], f32t)?;
        let w: Vec<f32> = (0..self.frames)
            .map(|i| if i < live { 1.0 } else { 0.0 })
            .collect();
        write_f32(&weights, &w);
        let out = predict(
            &self.embedding,
            &*provider(&[("fbank_features", &features), ("weights", &weights)])?,
            "embedding",
        )?;
        read(&*output(&out, "embedding")?)
    }
}
