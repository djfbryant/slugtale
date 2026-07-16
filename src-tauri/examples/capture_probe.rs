//! Diagnostic probe for the audio capture path (slugtale-8ul.8): records for a
//! few seconds through the same `CpalAudioRecorder` the app uses, then checks
//! the invariants garbled dictation would break — normalized duration matching
//! wall-clock time (dropped chunks / wrong channel count skew it), sane RMS
//! (gain), and no clipping — and dumps the normalized 16 kHz audio to a WAV so
//! it can be decoded offline with the decode_benchmark example.
//!
//! Usage:
//!   cargo run --example capture_probe -- [seconds] [out.wav]

use slugtale_lib::{AudioRecorder, CpalAudioRecorder};

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: f64 = args
        .next()
        .map(|value| value.parse().expect("seconds must be a number"))
        .unwrap_or(3.0);
    let out_path = args
        .next()
        .unwrap_or_else(|| "/tmp/slugtale-capture-probe.wav".to_string());

    let mut recorder = CpalAudioRecorder::new();
    let started = std::time::Instant::now();
    recorder.start().expect("start capture");
    std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    let audio = recorder.stop().expect("stop capture");
    let elapsed = started.elapsed().as_secs_f64();

    let expected = elapsed * audio.sample_rate_hz as f64;
    let got = audio.samples.len() as f64;
    let rms = (audio.samples.iter().map(|s| (s * s) as f64).sum::<f64>()
        / got.max(1.0))
    .sqrt();
    let peak = audio
        .samples
        .iter()
        .fold(0.0f32, |acc, s| acc.max(s.abs()));
    let clipped = audio.samples.iter().filter(|s| s.abs() >= 0.999).count();

    println!("wall-clock capture: {elapsed:.2}s");
    println!(
        "normalized samples: {} at {} Hz = {:.2}s ({:+.1}% vs wall clock)",
        audio.samples.len(),
        audio.sample_rate_hz,
        got / audio.sample_rate_hz as f64,
        (got - expected) / expected * 100.0
    );
    println!("rms: {rms:.4}  peak: {peak:.4}  clipped samples: {clipped}");

    write_f32_wav(&out_path, audio.sample_rate_hz, &audio.samples);
    println!("wrote {out_path}");
}

fn write_f32_wav(path: &str, sample_rate: u32, samples: &[f32]) {
    let data_len = (samples.len() * 4) as u32;
    let byte_rate = sample_rate * 4;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&sample_rate.to_le_bytes());
    bytes.extend_from_slice(&byte_rate.to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes).expect("write wav");
}
