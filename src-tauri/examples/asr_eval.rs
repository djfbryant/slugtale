//! Run this binary **once per engine** — only `--whisper-model`, then
//! separately only `--parakeet-assets`, then separately `--apple-speech` — to
//! get a peak-memory reading attributable to one engine. Peak resident memory
//! is a process-wide, monotonically increasing OS counter (see
//! `peak_resident_memory_bytes` below), so a combined run's number reflects
//! whichever engines had already loaded, not one engine in isolation. Pass

use slugtale_lib::{
    AppleSpeechProvider, AsrRuntime, CapturedAudio, EngineAvailability, EscalationReason,
    LocalModelRef, LocalWhisperRuntime, ParakeetProvider, RoutingDiagnostics, SecondOpinionMode,
    SecondOpinionRouter, SpeedProfile, TranscriptionProvider, WhisperTranscriptionProvider,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const USAGE: &str = "\
Usage: asr_eval --corpus <dir>
                 [--whisper-model <path>] [--parakeet-assets <dir>] [--apple-speech]
                 [--routed] [--second-opinion-engine <parakeet|apple-speech>]
                 [--terms <path>] [--json]

Pass one or more of --whisper-model, --parakeet-assets, --apple-speech. Run
the harness once per engine (only one of those flags at a time) to isolate
that engine's peak-memory reading. Pass --routed together with
--whisper-model and exactly one second-opinion candidate (or both
--parakeet-assets and --apple-speech plus --second-opinion-engine to say
which) to measure the Second Opinion router.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecondOpinionCandidate {
    Parakeet,
    AppleSpeech,
}

struct Args {
    corpus: PathBuf,
    whisper_model: Option<PathBuf>,
    parakeet_assets: Option<PathBuf>,
    apple_speech: bool,
    routed: bool,
    second_opinion_engine: Option<SecondOpinionCandidate>,
    terms: Option<PathBuf>,
    json: bool,
}

fn parse_args() -> Args {
    let mut corpus = None;
    let mut whisper_model = None;
    let mut parakeet_assets = None;
    let mut apple_speech = false;
    let mut routed = false;
    let mut second_opinion_engine = None;
    let mut terms = None;
    let mut json = false;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--corpus" => {
                corpus = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| panic!("--corpus needs a directory\n\n{USAGE}")),
                ))
            }
            "--whisper-model" => {
                whisper_model = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| panic!("--whisper-model needs a path\n\n{USAGE}")),
                ))
            }
            "--parakeet-assets" => {
                parakeet_assets = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| panic!("--parakeet-assets needs a directory\n\n{USAGE}")),
                ))
            }
            "--apple-speech" => apple_speech = true,
            "--terms" => {
                terms = Some(PathBuf::from(
                    args.next().unwrap_or_else(|| panic!("--terms needs a path\n\n{USAGE}")),
                ))
            }
            "--routed" => routed = true,
            "--second-opinion-engine" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| panic!("--second-opinion-engine needs a value\n\n{USAGE}"));
                second_opinion_engine = Some(match value.as_str() {
                    "parakeet" => SecondOpinionCandidate::Parakeet,
                    "apple-speech" => SecondOpinionCandidate::AppleSpeech,
                    other => panic!("unknown --second-opinion-engine {other}\n\n{USAGE}"),
                });
            }
            "--json" => json = true,
            other => panic!("unknown argument: {other}\n\n{USAGE}"),
        }
    }

    let corpus = corpus.unwrap_or_else(|| panic!("--corpus is required\n\n{USAGE}"));
    if whisper_model.is_none() && parakeet_assets.is_none() && !apple_speech {
        panic!("pass at least one of --whisper-model, --parakeet-assets, or --apple-speech\n\n{USAGE}");
    }
    if routed {
        if whisper_model.is_none() {
            panic!("--routed needs --whisper-model as the primary engine\n\n{USAGE}");
        }
        let candidates = parakeet_assets.is_some() as u8 + apple_speech as u8;
        if candidates == 0 {
            panic!("--routed needs --parakeet-assets and/or --apple-speech as a second opinion\n\n{USAGE}");
        }
        if candidates > 1 && second_opinion_engine.is_none() {
            panic!(
                "both --parakeet-assets and --apple-speech were given; pass --second-opinion-engine to say which plays second\n\n{USAGE}"
            );
        }
    }

    Args {
        corpus,
        whisper_model,
        parakeet_assets,
        apple_speech,
        routed,
        second_opinion_engine,
        terms,
        json,
    }
}

struct Clip {
    id: String,
    samples: Vec<f32>,
    reference: String,
}

fn load_corpus(dir: &Path) -> Vec<Clip> {
    let mut wav_paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("could not read corpus directory {}: {error}", dir.display()))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("wav"))
        .collect();
    wav_paths.sort();

    let mut clips = Vec::with_capacity(wav_paths.len());
    for wav_path in wav_paths {
        let id = wav_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("clip")
            .to_string();

        let reference_path = wav_path.with_extension("txt");
        let reference = match std::fs::read_to_string(&reference_path) {
            Ok(text) => text.trim().to_string(),
            Err(error) => {
                eprintln!("skipping {id}: no reference transcript ({error})");
                continue;
            }
        };

        match read_f32_mono_wav(&wav_path) {
            Ok(samples) => clips.push(Clip { id, samples, reference }),
            Err(reason) => eprintln!("skipping {id}: {reason}"),
        }
    }
    clips
}

fn read_f32_mono_wav(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("could not read wav: {error}"))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".to_string());
    }

    let mut offset = 12;
    let mut format_ok = false;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body_end = (offset + 8 + chunk_size).min(bytes.len());
        let body = &bytes[offset + 8..body_end];

        match chunk_id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err("fmt chunk too short".to_string());
                }
                let format_tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
                let channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                let sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                if format_tag != 3 {
                    return Err("expected IEEE float WAV".to_string());
                }
                if channels != 1 {
                    return Err("expected mono".to_string());
                }
                if sample_rate != 16_000 {
                    return Err("expected 16 kHz".to_string());
                }
                if bits != 32 {
                    return Err("expected 32-bit float".to_string());
                }
                format_ok = true;
            }
            b"data" => {
                if !format_ok {
                    return Err("data chunk before fmt chunk".to_string());
                }
                return Ok(body
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                    .collect());
            }
            _ => {}
        }
        offset += 8 + chunk_size + (chunk_size & 1);
    }
    Err("no data chunk found".to_string())
}

fn load_terms(path: Option<&Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()))
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}


fn normalize_for_wer(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            normalized.extend(ch.to_lowercase());
        } else if ch.is_whitespace() {
            normalized.push(' ');
        }
    }
    normalized.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct WordErrorRate {
    substitutions: usize,
    insertions: usize,
    deletions: usize,
    reference_words: usize,
    rate: f64,
}

fn word_error_rate(reference: &str, hypothesis: &str) -> WordErrorRate {
    let reference_normalized = normalize_for_wer(reference);
    let hypothesis_normalized = normalize_for_wer(hypothesis);
    let r: Vec<&str> = reference_normalized.split_whitespace().collect();
    let h: Vec<&str> = hypothesis_normalized.split_whitespace().collect();
    let (rn, hn) = (r.len(), h.len());

    let mut dp = vec![vec![0usize; hn + 1]; rn + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=rn {
        for j in 1..=hn {
            dp[i][j] = if r[i - 1] == h[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j - 1].min(dp[i - 1][j]).min(dp[i][j - 1])
            };
        }
    }

    let (mut i, mut j) = (rn, hn);
    let (mut substitutions, mut insertions, mut deletions) = (0usize, 0usize, 0usize);
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && r[i - 1] == h[j - 1] {
            i -= 1;
            j -= 1;
        } else if i > 0 && j > 0 && dp[i][j] == dp[i - 1][j - 1] + 1 {
            substitutions += 1;
            i -= 1;
            j -= 1;
        } else if i > 0 && dp[i][j] == dp[i - 1][j] + 1 {
            deletions += 1;
            i -= 1;
        } else {
            insertions += 1;
            j -= 1;
        }
    }

    let edits = substitutions + insertions + deletions;
    let rate = if rn == 0 {
        if hn == 0 {
            0.0
        } else {
            1.0
        }
    } else {
        edits as f64 / rn as f64
    };

    WordErrorRate {
        substitutions,
        insertions,
        deletions,
        reference_words: rn,
        rate,
    }
}

fn percentile(samples: &[Duration], p: f64) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[index])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectorOutcome {
    Win,
    Loss,
    Tie,
}

fn classify_selector_outcome(primary_alone_wer: f64, routed_wer: f64) -> SelectorOutcome {
    const EPSILON: f64 = 1e-9;
    if routed_wer < primary_alone_wer - EPSILON {
        SelectorOutcome::Win
    } else if routed_wer > primary_alone_wer + EPSILON {
        SelectorOutcome::Loss
    } else {
        SelectorOutcome::Tie
    }
}

fn term_recall(reference: &str, hypothesis: &str, terms: &[String]) -> (usize, usize) {
    let normalized_reference = normalize_for_wer(reference);
    let normalized_hypothesis = normalize_for_wer(hypothesis);
    let mut present_in_reference = 0;
    let mut also_in_hypothesis = 0;
    for term in terms {
        let normalized_term = normalize_for_wer(term);
        if normalized_term.is_empty() {
            continue;
        }
        if contains_phrase(&normalized_reference, &normalized_term) {
            present_in_reference += 1;
            if contains_phrase(&normalized_hypothesis, &normalized_term) {
                also_in_hypothesis += 1;
            }
        }
    }
    (also_in_hypothesis, present_in_reference)
}

fn contains_phrase(haystack: &str, phrase: &str) -> bool {
    let haystack_words: Vec<&str> = haystack.split_whitespace().collect();
    let phrase_words: Vec<&str> = phrase.split_whitespace().collect();
    if phrase_words.is_empty() || phrase_words.len() > haystack_words.len() {
        return false;
    }
    haystack_words
        .windows(phrase_words.len())
        .any(|window| window == phrase_words.as_slice())
}

fn punctuation_sequence(text: &str) -> Vec<char> {
    text.chars().filter(|c| c.is_ascii_punctuation()).collect()
}

fn capitalization_sequence(text: &str) -> Vec<bool> {
    text.split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphabetic()))
        .map(|first_letter| first_letter.is_uppercase())
        .collect()
}

fn sequence_accuracy<T: PartialEq>(reference: &[T], hypothesis: &[T]) -> f64 {
    let (rn, hn) = (reference.len(), hypothesis.len());
    if rn == 0 {
        return if hn == 0 { 1.0 } else { 0.0 };
    }
    let mut dp = vec![vec![0usize; hn + 1]; rn + 1];
    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=rn {
        for j in 1..=hn {
            dp[i][j] = if reference[i - 1] == hypothesis[j - 1] {
                dp[i - 1][j - 1]
            } else {
                1 + dp[i - 1][j - 1].min(dp[i - 1][j]).min(dp[i][j - 1])
            };
        }
    }
    (1.0 - dp[rn][hn] as f64 / rn as f64).max(0.0)
}

fn escalation_label(reason: EscalationReason) -> &'static str {
    match reason {
        EscalationReason::EmptyTranscript => "empty-transcript",
        EscalationReason::LowConfidence => "low-confidence",
        EscalationReason::RepeatedPhrase => "repeated-phrase",
        EscalationReason::ImplausiblyShortForDuration => "implausibly-short-for-duration",
    }
}

fn describe_unavailable(availability: EngineAvailability) -> String {
    match availability {
        EngineAvailability::Available => "available".to_string(),
        EngineAvailability::Unavailable(reason) => reason.to_string(),
    }
}

fn cold_and_warm_latency_ms(latencies: &[Duration]) -> (Option<u128>, Option<u128>, Option<u128>) {
    let cold = latencies.first().map(|d| d.as_millis());
    let warm: &[Duration] = if latencies.len() > 1 { &latencies[1..] } else { &[] };
    (
        cold,
        percentile(warm, 50.0).map(|d| d.as_millis()),
        percentile(warm, 95.0).map(|d| d.as_millis()),
    )
}


#[derive(Debug, Clone, serde::Serialize)]
struct EngineReport {
    engine: String,
    clips_evaluated: usize,
    clips_unavailable_or_errored: usize,
    mean_wer: f64,
    empty_output_rate: f64,
    silence_hallucination_rate: f64,
    term_recall: Option<f64>,
    punctuation_accuracy: f64,
    capitalization_accuracy: f64,
    cold_latency_ms: Option<u128>,
    warm_p50_latency_ms: Option<u128>,
    warm_p95_latency_ms: Option<u128>,
}

fn empty_engine_report(label: &str, clip_count: usize) -> EngineReport {
    EngineReport {
        engine: label.to_string(),
        clips_evaluated: 0,
        clips_unavailable_or_errored: clip_count,
        mean_wer: 0.0,
        empty_output_rate: 0.0,
        silence_hallucination_rate: 0.0,
        term_recall: None,
        punctuation_accuracy: 0.0,
        capitalization_accuracy: 0.0,
        cold_latency_ms: None,
        warm_p50_latency_ms: None,
        warm_p95_latency_ms: None,
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct RoutedReport {
    clips_evaluated: usize,
    escalation_rate: f64,
    escalation_reason_counts: BTreeMap<String, usize>,
    selector_wins: usize,
    selector_losses: usize,
    selector_ties: usize,
    mean_wer: f64,
    cold_latency_ms: Option<u128>,
    warm_p50_latency_ms: Option<u128>,
    warm_p95_latency_ms: Option<u128>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct HarnessReport {
    corpus_clip_count: usize,
    engines: Vec<EngineReport>,
    routed: Option<RoutedReport>,
    peak_resident_memory_bytes: Option<u64>,
}

fn evaluate_engine(
    label: &str,
    provider: &dyn TranscriptionProvider,
    clips: &[Clip],
    terms: &[String],
) -> (EngineReport, Vec<Option<f64>>) {
    if !provider.availability().is_available() {
        eprintln!(
            "{label}: unavailable ({})",
            describe_unavailable(provider.availability())
        );
        return (empty_engine_report(label, clips.len()), vec![None; clips.len()]);
    }

    let mut wer_sum = 0.0;
    let mut evaluated = 0usize;
    let mut errored = 0usize;
    let mut empty_output = 0usize;
    let mut non_silence_clips = 0usize;
    let mut silence_clips = 0usize;
    let mut silence_hallucinations = 0usize;
    let mut terms_present = 0usize;
    let mut terms_recalled = 0usize;
    let mut punctuation_acc_sum = 0.0;
    let mut capitalization_acc_sum = 0.0;
    let mut latencies: Vec<Duration> = Vec::new();
    let mut per_clip_wer: Vec<Option<f64>> = Vec::with_capacity(clips.len());

    for clip in clips {
        let audio = CapturedAudio::mono_16khz(clip.samples.clone());
        match provider.transcribe(&audio) {
            Ok(result) => {
                let hypothesis = result.text();
                let reference_is_silent = clip.reference.trim().is_empty();

                if reference_is_silent {
                    silence_clips += 1;
                    if !hypothesis.trim().is_empty() {
                        silence_hallucinations += 1;
                    }
                    per_clip_wer.push(None);
                } else {
                    non_silence_clips += 1;
                    let wer = word_error_rate(&clip.reference, hypothesis);
                    wer_sum += wer.rate;
                    if hypothesis.trim().is_empty() {
                        empty_output += 1;
                    }
                    punctuation_acc_sum += sequence_accuracy(
                        &punctuation_sequence(&clip.reference),
                        &punctuation_sequence(hypothesis),
                    );
                    capitalization_acc_sum += sequence_accuracy(
                        &capitalization_sequence(&clip.reference),
                        &capitalization_sequence(hypothesis),
                    );
                    let (recalled, present) = term_recall(&clip.reference, hypothesis, terms);
                    terms_present += present;
                    terms_recalled += recalled;
                    per_clip_wer.push(Some(wer.rate));
                }

                latencies.push(result.latency);
                evaluated += 1;
            }
            Err(error) => {
                eprintln!("{label}: {} failed ({error})", clip.id);
                errored += 1;
                per_clip_wer.push(None);
            }
        }
    }

    let (cold_latency_ms, warm_p50_latency_ms, warm_p95_latency_ms) =
        cold_and_warm_latency_ms(&latencies);

    let report = EngineReport {
        engine: label.to_string(),
        clips_evaluated: evaluated,
        clips_unavailable_or_errored: errored,
        mean_wer: if non_silence_clips > 0 {
            wer_sum / non_silence_clips as f64
        } else {
            0.0
        },
        empty_output_rate: if non_silence_clips > 0 {
            empty_output as f64 / non_silence_clips as f64
        } else {
            0.0
        },
        silence_hallucination_rate: if silence_clips > 0 {
            silence_hallucinations as f64 / silence_clips as f64
        } else {
            0.0
        },
        term_recall: if terms_present > 0 {
            Some(terms_recalled as f64 / terms_present as f64)
        } else {
            None
        },
        punctuation_accuracy: if non_silence_clips > 0 {
            punctuation_acc_sum / non_silence_clips as f64
        } else {
            0.0
        },
        capitalization_accuracy: if non_silence_clips > 0 {
            capitalization_acc_sum / non_silence_clips as f64
        } else {
            0.0
        },
        cold_latency_ms,
        warm_p50_latency_ms,
        warm_p95_latency_ms,
    };

    (report, per_clip_wer)
}

fn evaluate_routed(
    primary: Arc<dyn TranscriptionProvider>,
    second: Arc<dyn TranscriptionProvider>,
    clips: &[Clip],
    primary_alone_wers: &[Option<f64>],
) -> RoutedReport {
    let decided: Arc<Mutex<Option<RoutingDiagnostics>>> = Arc::new(Mutex::new(None));
    let recorder = Arc::clone(&decided);
    let router = SecondOpinionRouter::new(primary, second, SecondOpinionMode::Automatic).observing(
        move |diagnostics| {
            *recorder
                .lock()
                .expect("routing decision slot was never locked across threads") = Some(diagnostics);
        },
    );

    let mut evaluated = 0usize;
    let mut escalations = 0usize;
    let mut reason_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut ties = 0usize;
    let mut wer_sum = 0.0;
    let mut non_silence_clips = 0usize;
    let mut latencies: Vec<Duration> = Vec::new();

    for (index, clip) in clips.iter().enumerate() {
        let audio = CapturedAudio::mono_16khz(clip.samples.clone());

        *decided
            .lock()
            .expect("routing decision slot was never locked across threads") = None;

        let Ok(selected) = router.transcribe(audio) else {
            continue;
        };
        let Some(diagnostics) = *decided
            .lock()
            .expect("routing decision slot was never locked across threads")
        else {
            continue;
        };

        evaluated += 1;
        latencies.push(Duration::from_millis(diagnostics.total_latency_ms));

        let reference_is_silent = clip.reference.trim().is_empty();
        if !reference_is_silent {
            non_silence_clips += 1;
            let routed_wer = word_error_rate(&clip.reference, &selected.text).rate;
            wer_sum += routed_wer;

            if diagnostics.escalation.is_some() {
                if let Some(Some(primary_wer)) = primary_alone_wers.get(index).copied() {
                    match classify_selector_outcome(primary_wer, routed_wer) {
                        SelectorOutcome::Win => wins += 1,
                        SelectorOutcome::Loss => losses += 1,
                        SelectorOutcome::Tie => ties += 1,
                    }
                }
            }
        }

        if let Some(escalation) = diagnostics.escalation {
            escalations += 1;
            *reason_counts
                .entry(escalation_label(escalation).to_string())
                .or_insert(0) += 1;
        }
    }

    let (cold_latency_ms, warm_p50_latency_ms, warm_p95_latency_ms) =
        cold_and_warm_latency_ms(&latencies);

    RoutedReport {
        clips_evaluated: evaluated,
        escalation_rate: if evaluated > 0 {
            escalations as f64 / evaluated as f64
        } else {
            0.0
        },
        escalation_reason_counts: reason_counts,
        selector_wins: wins,
        selector_losses: losses,
        selector_ties: ties,
        mean_wer: if non_silence_clips > 0 {
            wer_sum / non_silence_clips as f64
        } else {
            0.0
        },
        cold_latency_ms,
        warm_p50_latency_ms,
        warm_p95_latency_ms,
    }
}


#[cfg(unix)]
fn peak_resident_memory_bytes() -> Option<u64> {
    #[repr(C)]
    struct Timeval {
        tv_sec: i64,
        tv_usec: i64,
    }
    #[repr(C)]
    struct RUsage {
        ru_utime: Timeval,
        ru_stime: Timeval,
        ru_maxrss: i64,
        ru_ixrss: i64,
        ru_idrss: i64,
        ru_isrss: i64,
        ru_minflt: i64,
        ru_majflt: i64,
        ru_nswap: i64,
        ru_inblock: i64,
        ru_oublock: i64,
        ru_msgsnd: i64,
        ru_msgrcv: i64,
        ru_nsignals: i64,
        ru_nvcsw: i64,
        ru_nivcsw: i64,
    }
    const RUSAGE_SELF: i32 = 0;
    extern "C" {
        fn getrusage(who: i32, usage: *mut RUsage) -> i32;
    }

    let mut usage: RUsage = unsafe { std::mem::zeroed() };
    let status = unsafe { getrusage(RUSAGE_SELF, &mut usage) };
    if status != 0 {
        return None;
    }

    let raw = u64::try_from(usage.ru_maxrss).ok()?;
    if cfg!(target_os = "macos") {
        Some(raw)
    } else {
        Some(raw * 1024) // kibibytes on Linux (glibc and musl)
    }
}

#[cfg(not(unix))]
fn peak_resident_memory_bytes() -> Option<u64> {
    None
}


fn format_ms(ms: Option<u128>) -> String {
    match ms {
        Some(ms) => format!("{ms} ms"),
        None => "n/a".to_string(),
    }
}

fn print_report(
    clip_count: usize,
    engines: &[EngineReport],
    routed: Option<&RoutedReport>,
    peak_rss: Option<u64>,
    json: bool,
) {
    if json {
        let report = HarnessReport {
            corpus_clip_count: clip_count,
            engines: engines.to_vec(),
            routed: routed.cloned(),
            peak_resident_memory_bytes: peak_rss,
        };
        println!("{}", serde_json::to_string_pretty(&report).expect("serialize report"));
        return;
    }

    println!("=== asr_eval: {clip_count} clip(s) ===");
    println!("Aggregate statistics only — no transcript, reference, or audio content is ever printed.");

    for report in engines {
        println!("\n--- {} ---", report.engine);
        println!("  clips evaluated:            {}", report.clips_evaluated);
        println!("  clips unavailable/errored:  {}", report.clips_unavailable_or_errored);
        println!("  mean WER:                   {:.3}", report.mean_wer);
        println!("  empty-output rate:          {:.3}", report.empty_output_rate);
        println!("  silence hallucination rate: {:.3}", report.silence_hallucination_rate);
        match report.term_recall {
            Some(recall) => println!("  proper-term recall:         {recall:.3}"),
            None => println!("  proper-term recall:         n/a (no --terms, or no reference term matched)"),
        }
        println!("  punctuation accuracy:       {:.3}", report.punctuation_accuracy);
        println!("  capitalization accuracy:    {:.3}", report.capitalization_accuracy);
        println!("  cold latency:               {}", format_ms(report.cold_latency_ms));
        println!("  warm p50 latency:           {}", format_ms(report.warm_p50_latency_ms));
        println!("  warm p95 latency:           {}", format_ms(report.warm_p95_latency_ms));
    }

    if let Some(routed) = routed {
        println!("\n--- routed (second opinion) ---");
        println!("  clips evaluated:            {}", routed.clips_evaluated);
        println!("  escalation rate:            {:.3}", routed.escalation_rate);
        for (reason, count) in &routed.escalation_reason_counts {
            println!("    {reason}: {count}");
        }
        let selector_total = routed.selector_wins + routed.selector_losses + routed.selector_ties;
        println!(
            "  selector win/loss/tie:      {}/{}/{} (of {selector_total} escalated, scored clips)",
            routed.selector_wins, routed.selector_losses, routed.selector_ties
        );
        println!("  mean WER (routed):          {:.3}", routed.mean_wer);
        println!("  cold latency:               {}", format_ms(routed.cold_latency_ms));
        println!("  warm p50 latency:           {}", format_ms(routed.warm_p50_latency_ms));
        println!("  warm p95 latency:           {}", format_ms(routed.warm_p95_latency_ms));
    }

    match peak_rss {
        Some(bytes) => println!(
            "\npeak resident memory (process-wide, cumulative across whatever ran above): {:.1} MB",
            bytes as f64 / 1024.0 / 1024.0
        ),
        None => println!("\npeak resident memory: unavailable on this platform"),
    }
}

fn main() {
    let args = parse_args();
    let clips = load_corpus(&args.corpus);
    if clips.is_empty() {
        eprintln!(
            "no usable clips found in {} (need sibling *.wav/*.txt pairs)",
            args.corpus.display()
        );
        std::process::exit(1);
    }
    let terms = load_terms(args.terms.as_deref());

    let mut engine_reports: Vec<EngineReport> = Vec::new();

    let mut whisper_provider: Option<Arc<dyn TranscriptionProvider>> = None;
    let mut whisper_wers: Vec<Option<f64>> = vec![None; clips.len()];

    if let Some(model_path) = &args.whisper_model {
        let runtime = Arc::new(LocalWhisperRuntime::new(LocalModelRef::at(
            model_path.clone(),
        )));
        let provider: Arc<dyn TranscriptionProvider> = Arc::new(WhisperTranscriptionProvider::new(
            runtime,
            SpeedProfile::default(),
            None,
        ));
        let (report, wers) = evaluate_engine("whisper", provider.as_ref(), &clips, &terms);
        engine_reports.push(report);
        whisper_wers = wers;
        whisper_provider = Some(provider);
    }

    let mut parakeet_concrete: Option<Arc<ParakeetProvider>> = None;
    let mut parakeet_provider: Option<Arc<dyn TranscriptionProvider>> = None;

    if let Some(asset_dir) = &args.parakeet_assets {
        let concrete = Arc::new(ParakeetProvider::new(asset_dir.clone()));
        let provider: Arc<dyn TranscriptionProvider> = concrete.clone();
        let (report, _wers) = evaluate_engine("parakeet", provider.as_ref(), &clips, &terms);
        engine_reports.push(report);
        parakeet_concrete = Some(concrete);
        parakeet_provider = Some(provider);
    }

    let mut apple_speech_provider: Option<Arc<dyn TranscriptionProvider>> = None;

    if args.apple_speech {
        let provider: Arc<dyn TranscriptionProvider> = Arc::new(AppleSpeechProvider::new());
        let (report, _wers) = evaluate_engine("apple-speech", provider.as_ref(), &clips, &terms);
        engine_reports.push(report);
        apple_speech_provider = Some(provider);
    }

    let routed_report = if args.routed {
        let primary = whisper_provider
            .clone()
            .expect("--routed requires --whisper-model (checked in parse_args)");
        let second = match args.second_opinion_engine {
            Some(SecondOpinionCandidate::Parakeet) => parakeet_provider.clone(),
            Some(SecondOpinionCandidate::AppleSpeech) => apple_speech_provider.clone(),
            None => parakeet_provider.clone().or_else(|| apple_speech_provider.clone()),
        }
        .expect("--routed requires a second-opinion candidate (checked in parse_args)");
        Some(evaluate_routed(primary, second, &clips, &whisper_wers))
    } else {
        None
    };

    if let Some(provider) = parakeet_concrete {
        provider.shutdown();
    }

    let peak_rss = peak_resident_memory_bytes();

    print_report(clips.len(), &engine_reports, routed_report.as_ref(), peak_rss, args.json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_punctuation_and_whitespace() {
        assert_eq!(
            normalize_for_wer("Book the 2pm meeting, please."),
            "book the 2pm meeting please"
        );
        assert_eq!(normalize_for_wer("  multiple   spaces  "), "multiple spaces");
        assert_eq!(normalize_for_wer(""), "");
        assert_eq!(normalize_for_wer("don't"), "dont");
    }

    #[test]
    fn wer_of_empty_reference_and_empty_hypothesis_is_zero() {
        assert_eq!(word_error_rate("", "").rate, 0.0);
        assert_eq!(word_error_rate("   ", "").rate, 0.0);
    }

    #[test]
    fn wer_of_empty_reference_with_a_hypothesis_is_a_hallucination() {
        let wer = word_error_rate("", "hello there");
        assert_eq!(wer.rate, 1.0);
        assert_eq!(wer.insertions, 2);
        assert_eq!(wer.reference_words, 0);
    }

    #[test]
    fn wer_of_a_reference_with_an_empty_hypothesis_is_total_deletion() {
        let wer = word_error_rate("hello there friend", "");
        assert_eq!(wer.rate, 1.0);
        assert_eq!(wer.deletions, 3);
        assert_eq!(wer.reference_words, 3);
    }

    #[test]
    fn wer_of_an_all_substitution_hypothesis() {
        let wer = word_error_rate("book the meeting", "cook she greeting");
        assert_eq!(wer.substitutions, 3);
        assert_eq!(wer.insertions, 0);
        assert_eq!(wer.deletions, 0);
        assert_eq!(wer.rate, 1.0);
    }

    #[test]
    fn wer_counts_a_pure_insertion() {
        let wer = word_error_rate("book the meeting", "book the big meeting");
        assert_eq!(wer.insertions, 1);
        assert_eq!(wer.substitutions, 0);
        assert_eq!(wer.deletions, 0);
        assert!((wer.rate - (1.0 / 3.0)).abs() < 1e-9);
    }

    #[test]
    fn wer_counts_a_pure_deletion() {
        let wer = word_error_rate("book the big meeting", "book the meeting");
        assert_eq!(wer.deletions, 1);
        assert_eq!(wer.substitutions, 0);
        assert_eq!(wer.insertions, 0);
        assert!((wer.rate - 0.25).abs() < 1e-9);
    }

    #[test]
    fn wer_ignores_case_and_punctuation_differences() {
        assert_eq!(word_error_rate("Book the meeting.", "book the meeting").rate, 0.0);
    }

    #[test]
    fn percentile_handles_a_single_sample() {
        let samples = vec![Duration::from_millis(150)];
        assert_eq!(percentile(&samples, 50.0), Some(Duration::from_millis(150)));
        assert_eq!(percentile(&samples, 95.0), Some(Duration::from_millis(150)));
    }

    #[test]
    fn percentile_of_no_samples_is_none() {
        assert_eq!(percentile(&[], 50.0), None);
    }

    #[test]
    fn percentile_picks_the_expected_rank_on_a_small_sample() {
        let samples: Vec<Duration> = [40, 10, 30, 20]
            .iter()
            .map(|ms| Duration::from_millis(*ms))
            .collect();
        assert_eq!(percentile(&samples, 50.0), Some(Duration::from_millis(20)));
        assert_eq!(percentile(&samples, 95.0), Some(Duration::from_millis(40)));
    }

    #[test]
    fn selector_outcome_classifies_win_loss_and_tie() {
        assert_eq!(classify_selector_outcome(0.5, 0.2), SelectorOutcome::Win);
        assert_eq!(classify_selector_outcome(0.2, 0.5), SelectorOutcome::Loss);
        assert_eq!(classify_selector_outcome(0.3, 0.3), SelectorOutcome::Tie);
        assert_eq!(
            classify_selector_outcome(0.333_333_333, 0.333_333_334),
            SelectorOutcome::Tie
        );
    }

    #[test]
    fn sequence_accuracy_of_matching_sequences_is_one() {
        assert_eq!(sequence_accuracy(&['.', ','], &['.', ',']), 1.0);
    }

    #[test]
    fn sequence_accuracy_of_empty_reference_and_hypothesis_is_one() {
        assert_eq!(sequence_accuracy::<char>(&[], &[]), 1.0);
    }

    #[test]
    fn sequence_accuracy_of_empty_reference_with_a_hypothesis_is_zero() {
        assert_eq!(sequence_accuracy(&[], &['.']), 0.0);
    }

    #[test]
    fn capitalization_sequence_skips_tokens_with_no_letters() {
        assert_eq!(
            capitalization_sequence("Book 3 rooms for Tuesday"),
            vec![true, false, false, true]
        );
    }

    #[test]
    fn contains_phrase_matches_multi_word_terms_only_as_a_contiguous_run() {
        assert!(contains_phrase("meet the slugtale team", "the slugtale"));
        assert!(!contains_phrase("meet slugtale the team", "the slugtale"));
    }

    #[test]
    fn term_recall_counts_only_terms_actually_said() {
        let terms = vec!["Slugtale".to_string(), "Kubernetes".to_string()];
        let (recalled, present) = term_recall("meet the Slugtale team", "meet the slugtale team", &terms);
        assert_eq!(present, 1);
        assert_eq!(recalled, 1);
    }
}
