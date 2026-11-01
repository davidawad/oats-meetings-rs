#!/usr/bin/env python3
"""English STT model benchmark for the catalog (dot-4uzgol.67).

Accuracy on a fixed public subset: 200 LibriSpeech test-clean + 100 test-other
utterances, picked by an even stride over the sorted utterance ids (see subset.tsv).
One model per invocation; writes results/<model>.json and hyps/<model>.tsv.

    bench.py subset   <librispeech-dir>        # (re)write subset.tsv
    bench.py run      <librispeech-dir> <model> [--models-dir D] [--threads N]
    bench.py table                             # print the results table from results/*.json

Speed here is pod CPU (shared AMD EPYC, N threads): an order-of-magnitude figure only.
Apple Neural Engine / MLX speed is a PM live check on a Mac.
"""
import hashlib
import json
import os
import re
import sys
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
SUBSETS = (("test-clean", 200), ("test-other", 100))


# ---------------------------------------------------------------- corpus

def read_transcripts(split_dir):
    out = {}
    for f in sorted(split_dir.rglob("*.trans.txt")):
        for line in f.read_text().splitlines():
            uid, text = line.split(" ", 1)
            out[uid] = (f.parent / f"{uid}.flac", text)
    return out


def make_subset(libri):
    rows = []
    for split, n in SUBSETS:
        all_ = read_transcripts(Path(libri) / split)
        ids = sorted(all_)
        stride = len(ids) // n
        for uid in ids[::stride][:n]:
            path, text = all_[uid]
            sha = hashlib.sha256(path.read_bytes()).hexdigest()
            rows.append((split, uid, sha, text))
    with open(HERE / "subset.tsv", "w") as f:
        f.write("split\tutterance\tflac_sha256\tlicence\ttext\n")
        for split, uid, sha, text in rows:
            f.write(f"{split}\t{uid}\t{sha}\tCC-BY-4.0\t{text}\n")
    print(f"wrote {len(rows)} rows")


def load_subset(libri):
    rows = []
    lines = (HERE / "subset.tsv").read_text().splitlines()[1:]
    for line in lines:
        split, uid, sha, _lic, text = line.split("\t")
        spk, chap, _ = uid.split("-")
        path = Path(libri) / split / spk / chap / f"{uid}.flac"
        if hashlib.sha256(path.read_bytes()).hexdigest() != sha:
            sys.exit(f"sha256 mismatch: {path}")
        rows.append((split, uid, path, text))
    return rows


def load_audio(path):
    import soundfile as sf
    samples, rate = sf.read(str(path), dtype="float32")
    assert rate == 16000, path
    return samples


# ---------------------------------------------------------------- WER

_ONES = "zero one two three four five six seven eight nine ten eleven twelve thirteen fourteen " \
        "fifteen sixteen seventeen eighteen nineteen".split()
_TENS = "_ _ twenty thirty forty fifty sixty seventy eighty ninety".split()


def _num_words(n):
    if n < 20:
        return _ONES[n]
    if n < 100:
        return _TENS[n // 10] + ("" if n % 10 == 0 else " " + _ONES[n % 10])
    if n < 1000:
        rest = n % 100
        return _ONES[n // 100] + " hundred" + ("" if rest == 0 else " " + _num_words(rest))
    if n < 1_000_000:
        rest = n % 1000
        return _num_words(n // 1000) + " thousand" + ("" if rest == 0 else " " + _num_words(rest))
    return str(n)


def normalize(text):
    """Lower-case, spell out plain integers, drop punctuation (keep in-word apostrophes)."""
    t = text.lower().replace("’", "'")
    t = re.sub(r"(\d),(\d{3})", r"\1\2", t)
    t = re.sub(r"\d+", lambda m: " " + _num_words(int(m.group())) + " ", t)
    t = t.replace("-", " ")
    t = re.sub(r"[^a-z' ]", " ", t)
    t = re.sub(r"(?<![a-z])'|'(?![a-z])", " ", t)
    t = re.sub(r"\bmister\b", "mr", t)
    t = re.sub(r"\bmr\b", "mister", t)
    t = re.sub(r"\bmrs\b", "missus", t)
    t = re.sub(r"\bdr\b", "doctor", t)
    return " ".join(t.split())


def edits(ref, hyp):
    r, h = ref.split(), hyp.split()
    prev = list(range(len(h) + 1))
    for i in range(1, len(r) + 1):
        cur = [i] + [0] * len(h)
        for j in range(1, len(h) + 1):
            cur[j] = min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (r[i - 1] != h[j - 1]))
        prev = cur
    return prev[-1], len(r)


# ---------------------------------------------------------------- models

def fetch(models_dir, files):
    """files: list of (relative name, url, sha256). Downloads and verifies; returns the dir."""
    for name, url, sha in files:
        dest = Path(models_dir) / name
        if not dest.exists():
            dest.parent.mkdir(parents=True, exist_ok=True)
            tmp = dest.with_suffix(dest.suffix + ".part")
            urllib.request.urlretrieve(url, tmp)
            tmp.rename(dest)
        got = hashlib.sha256(dest.read_bytes()).hexdigest()
        if sha and got != sha:
            sys.exit(f"sha256 mismatch {dest}: {got}")
        if not sha:
            print(f"  sha256 {name} {got}")
    return Path(models_dir)


def catalog_files(model_id):
    cat = json.loads((HERE / "../../crates/core/data/stt-models.json").read_text())
    for m in cat["models"]:
        if m["id"] == model_id:
            return [(f"{m['dir']}/{f['name']}" if m["dir"] else f["name"], f["url"], f["sha256"])
                    for f in m["files"]]
    sys.exit(f"{model_id} not in the catalog")


def sherpa_parakeet(models_dir, threads, model_id):
    import sherpa_onnx
    files = catalog_files(model_id)
    d = fetch(models_dir, files)
    p = {Path(n).name: str(d / n) for n, _, _ in files}
    rec = sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=p["encoder.int8.onnx"], decoder=p["decoder.int8.onnx"],
        joiner=p["joiner.int8.onnx"], tokens=p["tokens.txt"],
        num_threads=threads, model_type="nemo_transducer", decoding_method="greedy_search")
    return sherpa_decode(rec), sum(Path(d / n).stat().st_size for n, _, _ in files)


def sherpa_moonshine(models_dir, threads, model_id):
    import sherpa_onnx
    files = catalog_files(model_id)
    d = fetch(models_dir, files)
    p = {Path(n).name: str(d / n) for n, _, _ in files}
    rec = sherpa_onnx.OfflineRecognizer.from_moonshine(
        preprocessor=p["preprocess.onnx"], encoder=p["encode.int8.onnx"],
        uncached_decoder=p["uncached_decode.int8.onnx"], cached_decoder=p["cached_decode.int8.onnx"],
        tokens=p["tokens.txt"], num_threads=threads)
    return sherpa_decode(rec), sum(Path(d / n).stat().st_size for n, _, _ in files)


def sherpa_decode(rec):
    def decode(samples):
        s = rec.create_stream()
        s.accept_waveform(16000, samples)
        rec.decode_stream(s)
        return s.result.text
    return decode


def whisper_ct2(models_dir, threads, repo):
    from faster_whisper import WhisperModel
    from huggingface_hub import snapshot_download
    d = snapshot_download(repo, cache_dir=str(models_dir))
    m = WhisperModel(d, device="cpu", compute_type="int8", cpu_threads=threads)
    size = sum(f.stat().st_size for f in Path(d).rglob("*") if f.is_file())

    def decode(samples):
        segs, _ = m.transcribe(samples, language="en", beam_size=1, without_timestamps=True,
                               condition_on_previous_text=False)
        return " ".join(s.text for s in segs)
    return decode, size


def hf_snapshot(models_dir, repo):
    from huggingface_hub import snapshot_download
    d = snapshot_download(repo, cache_dir=str(models_dir))
    return d, sum(f.stat().st_size for f in Path(d).rglob("*") if f.is_file())


def qwen3_asr(models_dir, threads, repo):
    # PyTorch venv: pip install torch (CPU wheel) qwen-asr
    import torch
    from qwen_asr import Qwen3ASRModel
    torch.set_num_threads(threads)
    d, size = hf_snapshot(models_dir, repo)
    m = Qwen3ASRModel.from_pretrained(d, dtype=torch.float32, device_map="cpu",
                                      max_inference_batch_size=1, max_new_tokens=256)

    def decode(samples):
        return m.transcribe(audio=(samples, 16000), language="English")[0].text
    return decode, size


# id -> (loader, arg, licence, note)
MODELS = {
    "parakeet-tdt-0.6b-v2": (sherpa_parakeet, "parakeet-tdt-0.6b-v2", "CC-BY-4.0",
                             "sherpa-onnx int8 export, catalog files"),
    "whisper-large-v3-turbo": (whisper_ct2, "mobiuslabsgmbh/faster-whisper-large-v3-turbo", "MIT",
                               "CTranslate2 int8 (faster-whisper), greedy; same weights as the "
                               "catalog's whisper.cpp large-v3-turbo-q5_0 but a different quantisation"),
    "moonshine-base-en": (sherpa_moonshine, "moonshine-base-en", "MIT",
                          "sherpa-onnx int8 export, catalog files"),
    "qwen3-asr-0.6b": (qwen3_asr, "Qwen/Qwen3-ASR-0.6B", "Apache-2.0",
                       "PyTorch fp32 CPU (qwen-asr, transformers backend), language forced to English"),
    "qwen3-asr-1.7b": (qwen3_asr, "Qwen/Qwen3-ASR-1.7B", "Apache-2.0",
                       "PyTorch fp32 CPU (qwen-asr, transformers backend), language forced to English"),
}


def run(libri, model, models_dir, threads):
    loader, arg, licence, note = MODELS[model]
    rows = load_subset(libri)
    t0 = time.time()
    decode, size = loader(models_dir, threads, arg)
    load_s = time.time() - t0
    # Each decoded utterance is appended to hyps/<model>.partial.tsv, so a killed run resumes.
    (HERE / "hyps").mkdir(exist_ok=True)
    partial = HERE / "hyps" / f"{model}.partial.tsv"
    done = {}
    if partial.exists():
        for line in partial.read_text().splitlines():
            uid, wall_s, audio_s, hyp = line.split("\t")
            done[uid] = (float(wall_s), float(audio_s), hyp)
    with open(partial, "a") as f:
        for split, uid, path, ref in rows:
            if uid in done:
                continue
            samples = load_audio(path)
            t = time.time()
            hyp = " ".join(decode(samples).split())
            done[uid] = (time.time() - t, len(samples) / 16000, hyp)
            f.write(f"{uid}\t{done[uid][0]:.4f}\t{done[uid][1]:.4f}\t{hyp}\n")
            f.flush()
    per = {}
    hyps = []
    audio_s = 0.0
    wall = 0.0
    for split, uid, path, ref in rows:
        w, a, hyp = done[uid]
        wall += w
        audio_s += a
        e, n = edits(normalize(ref), normalize(hyp))
        acc = per.setdefault(split, [0, 0])
        acc[0] += e
        acc[1] += n
        hyps.append((split, uid, e, n, hyp))
    import resource
    out = {
        "model": model, "licence": licence, "note": note, "bytes": size,
        "wer": {s: round(100 * e / n, 2) for s, (e, n) in per.items()},
        "ref_words": {s: n for s, (e, n) in per.items()},
        "utterances": len(rows), "audio_s": round(audio_s, 1),
        "rtf_pod_cpu": round(wall / audio_s, 4), "threads": threads,
        "load_s": round(load_s, 1),
        "peak_rss_mb_last_pass": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss // 1024,
    }
    (HERE / "results").mkdir(exist_ok=True)
    (HERE / "results" / f"{model}.json").write_text(json.dumps(out, indent=1) + "\n")
    with open(HERE / "hyps" / f"{model}.tsv", "w") as f:
        f.write("split\tutterance\tedits\tref_words\thypothesis\n")
        for r in hyps:
            f.write("\t".join(map(str, r)) + "\n")
    partial.unlink()
    print(json.dumps(out, indent=1))


def table():
    print("| model | test-clean WER | test-other WER | size MB | licence | RTF pod CPU | peak RSS MB |")
    print("|---|---|---|---|---|---|---|")
    for f in sorted((HERE / "results").glob("*.json")):
        r = json.loads(f.read_text())
        print(f"| {r['model']} | {r['wer'].get('test-clean')}% | {r['wer'].get('test-other')}% | "
              f"{r['bytes'] / 1e6:.0f} | {r['licence']} | {r['rtf_pod_cpu']} | {r.get('peak_rss_mb', r.get('peak_rss_mb_last_pass'))} |")


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "subset":
        make_subset(sys.argv[2])
    elif cmd == "run":
        args = sys.argv[2:]
        md = os.environ.get("BENCH_MODELS_DIR", "/tmp/stt-bench-models")
        th = int(os.environ.get("BENCH_THREADS", "8"))
        run(args[0], args[1], md, th)
    elif cmd == "table":
        table()
