# Third-party notices

Models and data this repo downloads (never commits) or ships derived data from. Each is fetched at
a pinned revision and checked against the SHA-256 in `crates/core/data/*.json` or `ci/`.

## Speaker-embedding models (stt.diarization, crates/core/data/speaker-models.json)

- **pyannote community-1 speaker embedding, CoreML conversion** (`pyannote-community-1-coreml`):
  WeSpeaker ResNet34 trained by pyannote (pyannote/speaker-diarization-community-1), converted to
  CoreML by Fluid Inference (FluidInference/speaker-diarization-coreml, as FluidAudio runs it).
  CC BY 4.0. Credits: pyannote.audio (Hervé Bredin et al.), WeSpeaker, BUT Speech@FIT, Fluid
  Inference.
- **WeSpeaker ResNet34-LM, VoxCeleb** (`wespeaker-resnet34-lm`): WeSpeaker (wenet-e2e/wespeaker),
  ONNX export by k2-fsa/sherpa-onnx. CC BY 4.0.
- **NVIDIA NeMo TitaNet-Small** (`titanet-small`): NVIDIA, ONNX export by k2-fsa/sherpa-onnx.
  CC BY 4.0.

Licence text: https://creativecommons.org/licenses/by/4.0/

## Test data

- **AMI Meeting Corpus** (tests/fixtures/diarize-ami): © University of Edinburgh / AMI
  Consortium, CC BY 4.0 (https://groups.inf.ed.ac.uk/ami/corpus/license.shtml). The `.rttm` speaker
  turns are from pyannote/AMI-diarization-setup (Apache-2.0), derived from the AMI annotations.
