
use slugtale_lib::{CpalAudioRecorder, DictationRecorder};
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

    let cold_start = {
        let mut recorder = CpalAudioRecorder::new();
        time_start(&mut recorder)
    };
    println!("cold start: {:.1} ms", millis(cold_start));

    let (prepare_time, prepared_start) = {
        let mut recorder = CpalAudioRecorder::new();
        let started = Instant::now();
        recorder
            .prepare()
            .expect("prepare capture before the first Hotkey");
        let preparation = started.elapsed();
        let start = time_start(&mut recorder);
        (preparation, start)
    };
    println!(
        "prepare (idle): {:.1} ms -> prepared start: {:.1} ms",
        millis(prepare_time),
        millis(prepared_start)
    );

    let mut recorder = CpalAudioRecorder::new();
    recorder.prepare().expect("prepare capture");
    let mut startup_times = Vec::with_capacity(starts);
    for index in 0..starts {
        let started = Instant::now();
        recorder.start().expect("start capture");
        let elapsed = started.elapsed();
        startup_times.push(elapsed);
        println!("start {}: {:.1} ms", index + 1, millis(elapsed));
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

fn time_start(recorder: &mut CpalAudioRecorder) -> Duration {
    let started = Instant::now();
    recorder.start().expect("start capture");
    let elapsed = started.elapsed();
    std::thread::sleep(Duration::from_millis(20));
    let _ = recorder.stop().expect("stop capture");
    elapsed
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
