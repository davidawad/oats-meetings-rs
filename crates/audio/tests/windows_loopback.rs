//! WASAPI loopback proof (dot-4uzgol.7). Windows only; needs an output device (a real or virtual
//! sound card). NOT run in the Linux pods: it runs in the Windows VM of dot-4uzgol.16, with
//! `OATS_AUDIO_LIVE=1 cargo test -p oats-audio --test windows_loopback -- --nocapture`.
//! Plays a 440 Hz tone on the default output with cpal, records the loopback for 5 s through the
//! `Recorder`, and asserts the loopback channel is non-silent and the duration is >= 4 s.
#![cfg(windows)]

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use oats_audio::cpal_backend::CpalBackend;
use oats_audio::{recorder, wav, AudioSettings, CaptureBackend, DeviceKind};

fn timing() -> oats_audio::Timing {
    oats_audio::Timing { open_timeout: Duration::from_secs(15), poll: Duration::from_millis(25) }
}

fn live() -> bool {
    let on = std::env::var("OATS_AUDIO_LIVE").ok().as_deref() == Some("1");
    if !on {
        eprintln!("windows_loopback: skipped (set OATS_AUDIO_LIVE=1 on a Windows machine with an output device)");
    }
    on
}

#[test]
fn devices_list_has_a_loopback_source() {
    if !live() {
        return;
    }
    let devs = CpalBackend::new(timing()).list_devices().unwrap();
    assert!(devs.iter().any(|d| d.kind == DeviceKind::Loopback && d.default), "{devs:?}");
}

#[test]
fn five_second_wasapi_loopback_is_non_silent() {
    if !live() {
        return;
    }
    // Tone on the default output device.
    let host = cpal::default_host();
    let out = host.default_output_device().expect("no default output device");
    let cfg = out.default_output_config().unwrap();
    let (rate, channels) = (cfg.sample_rate().0 as f32, usize::from(cfg.channels()));
    let mut n = 0u64;
    let stream = out
        .build_output_stream(
            &cfg.into(),
            move |data: &mut [f32], _| {
                for frame in data.chunks_mut(channels) {
                    let v = ((n as f32 / rate) * 440.0 * std::f32::consts::TAU).sin() * 0.5;
                    frame.iter_mut().for_each(|s| *s = v);
                    n += 1;
                }
            },
            |e| eprintln!("output stream error: {e}"),
            None,
        )
        .unwrap();
    stream.play().unwrap();
    std::thread::sleep(Duration::from_millis(500));

    let d = std::env::temp_dir().join(format!("oats-win-loopback-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let path = d.join("loopback.wav");
    // System audio only: a Windows VM may have no microphone.
    let st = AudioSettings { mic_enabled: false, ..AudioSettings::new(timing()) };
    let rec = recorder::start(&CpalBackend::new(timing()), &st, &path).unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let s = rec.stop().unwrap();
    drop(stream);
    let w = wav::read(&path).unwrap();
    eprintln!("windows_loopback: {:.2}s rms {:.3} tone {:.0} Hz", w.duration_secs(), w.rms(0), w.tone_hz(0));
    assert!(s.duration_ms >= 4000, "duration {}", s.duration_ms);
    assert!(w.rms(0) > 0.05, "loopback rms {}", w.rms(0));
    assert!((w.tone_hz(0) - 440.0).abs() < 20.0, "tone {}", w.tone_hz(0));
}
