//! Measures repeated microphone startup through the same `CpalAudioRecorder`
//! used by the app (slugtale-op3). No captured audio is persisted.
//!
//! Usage:
//!   cargo run --example startup_probe -- [starts] [max-warm-start-ms]

use slugtale_lib::{AudioRecorder, CpalAudioRecorder};
use std::time::{Duration, Instant};

fn main() {
    let mut args = std::env::args().skip(1);
    let starts: usize = args
        .next()
        .map(|value| value.parse().expect("starts must be an integer"))
        .unwrap_or(6);
    let max_warm_start_ms: u128 = args
        .next()
        .map(|value| value.parse().expect("max-warm-start-ms must be an integer"))
        .unwrap_or(75);

    assert!(starts >= 2, "at least two starts are required");

    let mut recorder = CpalAudioRecorder::new();
    let mut startup_times = Vec::with_capacity(starts);
    for index in 0..starts {
        let started = Instant::now();
        recorder.start().expect("start capture");
        let elapsed = started.elapsed();
        startup_times.push(elapsed);
        println!("start {}: {:.1} ms", index + 1, millis(elapsed));
        // Let the real callback run, then end through the normal completed-
        // dictation path. This verifies that the retained stream survives
        // `stop` draining the capture buffer, not only the cheaper cancel path.
        std::thread::sleep(Duration::from_millis(20));
        let _ = recorder.stop().expect("stop capture");
    }

    let slowest_warm_start = startup_times[1..]
        .iter()
        .copied()
        .max()
        .expect("warm starts");
    println!(
        "slowest warm start: {:.1} ms (limit: {max_warm_start_ms} ms)",
        millis(slowest_warm_start)
    );

    if slowest_warm_start.as_millis() > max_warm_start_ms {
        eprintln!("warm microphone startup exceeded the responsiveness limit");
        std::process::exit(1);
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
