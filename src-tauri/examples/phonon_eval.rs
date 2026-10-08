//! Local fixture harness. Never use private recordings with --show-text.
use slugtale_lib::{
    CapturedAudio, EngineAssetLifecycle, EngineTranscriber, ParakeetProvider, PhononProvider,
    PHONON_2,
};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};

fn read_wav(path: &Path) -> Result<CapturedAudio, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("expected RIFF WAV".into());
    }
    let mut offset = 12;
    let mut format = None;
    let mut samples = None;
    while offset + 8 <= bytes.len() {
        let n = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into()?) as usize;
        let start = offset + 8;
        let data = bytes.get(start..start + n).ok_or("truncated WAV")?;
        match &bytes[offset..offset + 4] {
            b"fmt " if data.len() >= 16 => {
                format = Some((
                    u16::from_le_bytes(data[0..2].try_into()?),
                    u16::from_le_bytes(data[2..4].try_into()?),
                    u32::from_le_bytes(data[4..8].try_into()?),
                    u16::from_le_bytes(data[14..16].try_into()?),
                ));
            }
            b"data" => samples = Some(data),
            _ => {}
        }
        offset = start + n + n % 2;
    }
    if format != Some((1, 1, 16_000, 16)) {
        return Err("expected mono 16 kHz PCM16 WAV".into());
    }
    let data = samples.ok_or("no WAV samples")?;
    if data.len() % 2 != 0 {
        return Err("incomplete PCM sample".into());
    }
    Ok(CapturedAudio {
        sample_rate_hz: 16_000,
        samples: data
            .chunks_exact(2)
            .map(|p| i16::from_le_bytes([p[0], p[1]]) as f32 / 32768.0)
            .collect(),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let backend = args.next().ok_or("Usage: phonon_eval <mlx|onnx> <models-directory|onnx-assets> [--install] [--show-text] [wav ...]")?;
    let root = PathBuf::from(args.next().ok_or("missing model directory")?);
    let provider = match backend.as_str() {
        "mlx" => PhononProvider::new(&root),
        "onnx" => PhononProvider::Onnx(ParakeetProvider::for_model(&PHONON_2, root)),
        _ => return Err("expected mlx or onnx".into()),
    };
    let args: Vec<_> = args.collect();
    if args.iter().any(|a| a == "--install") {
        provider.install_assets(&mut |_| {})?;
    }
    println!("metadata: {}", provider.metadata().revision);
    println!("availability: {:?}", provider.availability());
    let start = Instant::now();
    provider.warm_up()?;
    println!(
        "cold_load_and_warm_up_ms: {:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    for name in args.iter().filter(|a| !a.starts_with("--")) {
        let audio = read_wav(Path::new(name))?;
        let seconds = audio.samples.len() as f64 / 16000.0;
        let mut times = Vec::new();
        for pass in 0..4 {
            let result = provider.transcribe(&audio)?;
            let ms = result.latency.as_secs_f64() * 1000.0;
            println!(
                "{} pass={} audio_s={:.3} elapsed_ms={:.3} words={}",
                Path::new(name).file_name().unwrap().to_string_lossy(),
                pass,
                seconds,
                ms,
                result.text().split_whitespace().count()
            );
            if pass == 0 && args.iter().any(|a| a == "--show-text") {
                println!("fixture_text: {}", result.text());
            }
            if pass > 0 {
                times.push(ms);
            }
        }
        times.sort_by(f64::total_cmp);
        println!("warm_median_ms: {:.3}", times[1]);
    }
    Ok(())
}
