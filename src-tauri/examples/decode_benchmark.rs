//! Measures transcription latency and output text for the decode strategies the
//! Transcription Speed Profile chooses between (see docs/research/whisper-decode-benchmark.md).
//!
//! Usage:
//!   cargo run --release --example decode_benchmark \
//!     --features local-whisper-runtime[,local-whisper-runtime-metal] -- \
//!     <model.bin> <clip.wav> [more clips ...]
//!
//! Clips must be 16 kHz mono float32 WAV (e.g. `afconvert -f WAVE -d LEF32@16000 -c 1`).

use std::time::Instant;

fn main() {
    let mut args = std::env::args().skip(1);
    let model_path = args.next().expect("first argument: path to ggml model");
    let clip_paths: Vec<String> = args.collect();
    assert!(
        !clip_paths.is_empty(),
        "pass at least one 16 kHz mono f32 WAV clip"
    );

    let context = whisper_rs::WhisperContext::new_with_params(
        &model_path,
        whisper_rs::WhisperContextParameters::default(),
    )
    .expect("load model");

    let all_threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1);

    let strategies: Vec<(String, whisper_rs::SamplingStrategy, i32)> = vec![
        (
            format!("greedy best_of=1, {all_threads} threads"),
            whisper_rs::SamplingStrategy::Greedy { best_of: 1 },
            all_threads,
        ),
        (
            "greedy best_of=1, 4 threads (whisper default)".to_string(),
            whisper_rs::SamplingStrategy::Greedy { best_of: 1 },
            4,
        ),
        (
            format!("beam_size=2, {all_threads} threads"),
            whisper_rs::SamplingStrategy::BeamSearch {
                beam_size: 2,
                patience: -1.0,
            },
            all_threads,
        ),
        (
            format!("beam_size=5 (previous default), {all_threads} threads"),
            whisper_rs::SamplingStrategy::BeamSearch {
                beam_size: 5,
                patience: -1.0,
            },
            all_threads,
        ),
    ];

    for clip_path in &clip_paths {
        let samples = read_f32_mono_wav(clip_path);
        let clip_seconds = samples.len() as f64 / 16_000.0;
        println!("\n=== {clip_path} ({clip_seconds:.1}s) ===");

        for (label, strategy, n_threads) in &strategies {
            let mut state = context.create_state().expect("create state");
            let mut params = whisper_rs::FullParams::new(strategy.clone());
            params.set_n_threads(*n_threads);
            params.set_language(Some("en"));
            params.set_translate(false);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);

            // Warm run first so model upload / cache effects don't skew the timing,
            // then time the median-ish second run.
            state.full(params.clone(), &samples).expect("decode");
            let started = Instant::now();
            let mut state = context.create_state().expect("create state");
            state.full(params, &samples).expect("decode");
            let elapsed = started.elapsed();

            let text = state
                .as_iter()
                .map(|segment| segment.to_string())
                .collect::<Vec<_>>()
                .join("")
                .trim()
                .to_string();
            println!("{label}: {:.0} ms | {text}", elapsed.as_secs_f64() * 1000.0);
        }
    }
}

/// Minimal RIFF/WAVE reader for the exact format the benchmark clips use:
/// 16 kHz mono IEEE float32.
fn read_f32_mono_wav(path: &str) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("read wav");
    assert_eq!(&bytes[0..4], b"RIFF", "{path}: not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "{path}: not a WAVE file");

    let mut offset = 12;
    let mut format_ok = false;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = &bytes[offset + 8..(offset + 8 + chunk_size).min(bytes.len())];

        match chunk_id {
            b"fmt " => {
                let format_tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
                let channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                let sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                assert_eq!(format_tag, 3, "{path}: expected IEEE float WAV");
                assert_eq!(channels, 1, "{path}: expected mono");
                assert_eq!(sample_rate, 16_000, "{path}: expected 16 kHz");
                assert_eq!(bits, 32, "{path}: expected 32-bit float");
                format_ok = true;
            }
            b"data" => {
                assert!(format_ok, "{path}: data chunk before fmt chunk");
                return body
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect();
            }
            _ => {}
        }
        offset += 8 + chunk_size + (chunk_size & 1);
    }
    panic!("{path}: no data chunk found");
}
