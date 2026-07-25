//! Development-only local ASR corpus recorder and evaluation rig.

use slugtale_lib::{
    AsrRuntime, AudioRecorder, CapturedAudio, CpalAudioRecorder, LocalWhisperRuntime,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const CORPUS_SCHEMA_VERSION: u32 = 1;
const ADAPTER_SCHEMA_VERSION: u32 = 1;
const RUN_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CorpusManifest {
    schema_version: u32,
    name: String,
    clips: Vec<ClipSpec>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ClipSpec {
    id: String,
    expected_text: String,
    category: String,
    recording_condition: String,
    wav_path: PathBuf,
    #[serde(default)]
    proper_terms: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AdapterRequest {
    schema_version: u32,
    clip_id: String,
    wav_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct EngineIdentity {
    engine: String,
    model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AdapterError {
    code: String,
    detail: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AdapterResult {
    schema_version: u32,
    clip_id: String,
    engine: EngineIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hypothesis: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confidence: Option<f64>,
    latency_ms: f64,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    timings_ms: std::collections::BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<AdapterError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct EvaluationRun {
    schema_version: u32,
    run_id: String,
    engine: EngineIdentity,
    results: Vec<AdapterResult>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct EngineAggregate {
    engine: EngineIdentity,
    clips_total: usize,
    clips_scored: usize,
    errors: usize,
    normalized_wer: Option<f64>,
    proper_term_recall: Option<f64>,
    punctuation_accuracy: Option<f64>,
    capitalization_accuracy: Option<f64>,
    silence_hallucination_rate: Option<f64>,
    latency_p50_ms: Option<f64>,
    latency_p95_ms: Option<f64>,
    confidence_ece: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct PairAggregate {
    first_engine: EngineIdentity,
    second_engine: EngineIdentity,
    clips_compared: usize,
    normalized_agreement_rate: Option<f64>,
    disagreement_clips: usize,
    first_disagreement_wer: Option<f64>,
    second_disagreement_wer: Option<f64>,
    oracle_wer: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct AggregateReport {
    schema_version: u32,
    corpus_name: String,
    clip_count: usize,
    engines: Vec<EngineAggregate>,
    pairs: Vec<PairAggregate>,
}

fn normalize_for_wer(text: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.extend(character.to_lowercase());
            pending_space = false;
        } else if character.is_whitespace() {
            pending_space = true;
        }
    }
    normalized
}

fn edit_distance<T: Eq>(reference: &[T], hypothesis: &[T]) -> usize {
    let mut previous: Vec<usize> = (0..=hypothesis.len()).collect();
    let mut current = vec![0; hypothesis.len() + 1];
    for (reference_index, reference_item) in reference.iter().enumerate() {
        current[0] = reference_index + 1;
        for (hypothesis_index, hypothesis_item) in hypothesis.iter().enumerate() {
            current[hypothesis_index + 1] = if reference_item == hypothesis_item {
                previous[hypothesis_index]
            } else {
                1 + previous[hypothesis_index]
                    .min(current[hypothesis_index])
                    .min(previous[hypothesis_index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[hypothesis.len()]
}

fn word_errors(reference: &str, hypothesis: &str) -> (usize, usize) {
    let reference = normalize_for_wer(reference);
    let hypothesis = normalize_for_wer(hypothesis);
    let reference_words: Vec<&str> = reference.split_whitespace().collect();
    let hypothesis_words: Vec<&str> = hypothesis.split_whitespace().collect();
    (
        edit_distance(&reference_words, &hypothesis_words),
        reference_words.len(),
    )
}

fn punctuation_sequence(text: &str) -> Vec<(usize, char)> {
    let mut word_index = 0usize;
    let mut in_word = false;
    let mut punctuation = Vec::new();
    for character in text.chars() {
        if character.is_alphanumeric() {
            if !in_word {
                word_index += 1;
            }
            in_word = true;
        } else {
            if matches!(character, '.' | ',' | '?' | '!' | ':' | ';') {
                punctuation.push((word_index, character));
            }
            if !matches!(character, '\'' | '-') {
                in_word = false;
            }
        }
    }
    punctuation
}

fn capitalization_sequence(text: &str) -> Vec<(String, bool)> {
    text.split_whitespace()
        .filter_map(|token| {
            let capitalized = token
                .chars()
                .find(|character| character.is_alphabetic())
                .map(|character| character.is_uppercase())?;
            let normalized = normalize_for_wer(token);
            (!normalized.is_empty()).then_some((normalized, capitalized))
        })
        .collect()
}

fn contains_normalized_phrase(text: &str, phrase: &str) -> bool {
    let text = normalize_for_wer(text);
    let phrase = normalize_for_wer(phrase);
    if phrase.is_empty() {
        return false;
    }
    let text_words: Vec<&str> = text.split_whitespace().collect();
    let phrase_words: Vec<&str> = phrase.split_whitespace().collect();
    text_words
        .windows(phrase_words.len())
        .any(|window| window == phrase_words)
}

fn percentile(values: &[f64], percentile: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let rank = ((percentile / 100.0) * values.len() as f64).ceil() as usize;
    Some(values[rank.saturating_sub(1).min(values.len() - 1)])
}

fn validate_run(manifest: &CorpusManifest, run: &EvaluationRun) -> Result<(), String> {
    if run.schema_version != RUN_SCHEMA_VERSION {
        return Err(format!("run {} has unsupported schema version", run.run_id));
    }
    if run.run_id.trim().is_empty()
        || run.engine.engine.trim().is_empty()
        || run.engine.model.trim().is_empty()
    {
        return Err("run and engine/model identities must not be empty".to_string());
    }
    if run.results.len() != manifest.clips.len() {
        return Err(format!(
            "run {} has {} results for {} manifest clips",
            run.run_id,
            run.results.len(),
            manifest.clips.len()
        ));
    }
    let manifest_ids: std::collections::HashSet<&str> =
        manifest.clips.iter().map(|clip| clip.id.as_str()).collect();
    let mut result_ids = std::collections::HashSet::new();
    for result in &run.results {
        if result.schema_version != ADAPTER_SCHEMA_VERSION {
            return Err(format!(
                "result {} has unsupported adapter schema",
                result.clip_id
            ));
        }
        if result.engine != run.engine {
            return Err(format!("result {} changed engine identity", result.clip_id));
        }
        if !manifest_ids.contains(result.clip_id.as_str()) || !result_ids.insert(&result.clip_id) {
            return Err(format!(
                "run {} has an unknown or duplicate clip ID",
                run.run_id
            ));
        }
        if result.hypothesis.is_some() == result.error.is_some() {
            return Err(format!(
                "result {} must contain exactly one of hypothesis or error",
                result.clip_id
            ));
        }
        if !result.latency_ms.is_finite() || result.latency_ms < 0.0 {
            return Err(format!("result {} has invalid latency", result.clip_id));
        }
        if let Some(confidence) = result.confidence {
            if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                return Err(format!("result {} has invalid confidence", result.clip_id));
            }
        }
        if result
            .timings_ms
            .values()
            .any(|timing| !timing.is_finite() || *timing < 0.0)
        {
            return Err(format!("result {} has an invalid timing", result.clip_id));
        }
    }
    Ok(())
}

fn confidence_ece(samples: &[(f64, bool)]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut bins: [(usize, f64, usize); 10] = [(0, 0.0, 0); 10];
    for (confidence, correct) in samples {
        let index = ((*confidence * 10.0).floor() as usize).min(9);
        bins[index].0 += 1;
        bins[index].1 += confidence;
        bins[index].2 += usize::from(*correct);
    }
    Some(
        bins.iter()
            .filter(|(count, _, _)| *count > 0)
            .map(|(count, confidence_sum, correct)| {
                let weight = *count as f64 / samples.len() as f64;
                weight * (confidence_sum / *count as f64 - *correct as f64 / *count as f64).abs()
            })
            .sum(),
    )
}

fn aggregate_engine(manifest: &CorpusManifest, run: &EvaluationRun) -> EngineAggregate {
    let by_id: std::collections::HashMap<&str, &AdapterResult> = run
        .results
        .iter()
        .map(|result| (result.clip_id.as_str(), result))
        .collect();
    let mut word_edit_total = 0usize;
    let mut reference_word_total = 0usize;
    let mut term_present = 0usize;
    let mut term_recalled = 0usize;
    let mut punctuation_edits = 0usize;
    let mut punctuation_total = 0usize;
    let mut capitalization_edits = 0usize;
    let mut capitalization_total = 0usize;
    let mut silence_clips = 0usize;
    let mut silence_hallucinations = 0usize;
    let mut latencies = Vec::new();
    let mut confidences = Vec::new();
    let mut scored = 0usize;

    for clip in &manifest.clips {
        let result = by_id[clip.id.as_str()];
        let Some(hypothesis) = result.hypothesis.as_deref() else {
            continue;
        };
        scored += 1;
        latencies.push(result.latency_ms);
        let reference_normalized = normalize_for_wer(&clip.expected_text);
        let hypothesis_normalized = normalize_for_wer(hypothesis);
        if reference_normalized.is_empty() {
            silence_clips += 1;
            silence_hallucinations += usize::from(!hypothesis_normalized.is_empty());
        } else {
            let (edits, words) = word_errors(&clip.expected_text, hypothesis);
            word_edit_total += edits;
            reference_word_total += words;

            let reference_punctuation = punctuation_sequence(&clip.expected_text);
            let hypothesis_punctuation = punctuation_sequence(hypothesis);
            punctuation_edits += edit_distance(&reference_punctuation, &hypothesis_punctuation);
            punctuation_total += reference_punctuation
                .len()
                .max(hypothesis_punctuation.len());

            let reference_capitalization = capitalization_sequence(&clip.expected_text);
            let hypothesis_capitalization = capitalization_sequence(hypothesis);
            capitalization_edits +=
                edit_distance(&reference_capitalization, &hypothesis_capitalization);
            capitalization_total += reference_capitalization
                .len()
                .max(hypothesis_capitalization.len());
        }
        for term in &clip.proper_terms {
            if contains_normalized_phrase(&clip.expected_text, term) {
                term_present += 1;
                term_recalled += usize::from(contains_normalized_phrase(hypothesis, term));
            }
        }
        if let Some(confidence) = result.confidence {
            confidences.push((confidence, reference_normalized == hypothesis_normalized));
        }
    }

    let accuracy = |edits: usize, total: usize| {
        (total > 0).then(|| (1.0 - edits as f64 / total as f64).clamp(0.0, 1.0))
    };
    EngineAggregate {
        engine: run.engine.clone(),
        clips_total: manifest.clips.len(),
        clips_scored: scored,
        errors: manifest.clips.len() - scored,
        normalized_wer: (reference_word_total > 0)
            .then(|| word_edit_total as f64 / reference_word_total as f64),
        proper_term_recall: (term_present > 0).then(|| term_recalled as f64 / term_present as f64),
        punctuation_accuracy: accuracy(punctuation_edits, punctuation_total),
        capitalization_accuracy: accuracy(capitalization_edits, capitalization_total),
        silence_hallucination_rate: (silence_clips > 0)
            .then(|| silence_hallucinations as f64 / silence_clips as f64),
        latency_p50_ms: percentile(&latencies, 50.0),
        latency_p95_ms: percentile(&latencies, 95.0),
        confidence_ece: confidence_ece(&confidences),
    }
}

fn aggregate_pair(
    manifest: &CorpusManifest,
    first: &EvaluationRun,
    second: &EvaluationRun,
) -> PairAggregate {
    let first_by_id: std::collections::HashMap<&str, &AdapterResult> = first
        .results
        .iter()
        .map(|result| (result.clip_id.as_str(), result))
        .collect();
    let second_by_id: std::collections::HashMap<&str, &AdapterResult> = second
        .results
        .iter()
        .map(|result| (result.clip_id.as_str(), result))
        .collect();
    let mut compared = 0usize;
    let mut agreed = 0usize;
    let mut disagreements = 0usize;
    let mut disagreement_reference_words = 0usize;
    let mut first_disagreement_edits = 0usize;
    let mut second_disagreement_edits = 0usize;
    let mut oracle_reference_words = 0usize;
    let mut oracle_edits = 0usize;

    for clip in &manifest.clips {
        let Some(first_hypothesis) = first_by_id[clip.id.as_str()].hypothesis.as_deref() else {
            continue;
        };
        let Some(second_hypothesis) = second_by_id[clip.id.as_str()].hypothesis.as_deref() else {
            continue;
        };
        compared += 1;
        let first_normalized = normalize_for_wer(first_hypothesis);
        let second_normalized = normalize_for_wer(second_hypothesis);
        agreed += usize::from(first_normalized == second_normalized);

        let (first_edits, reference_words) = word_errors(&clip.expected_text, first_hypothesis);
        let (second_edits, _) = word_errors(&clip.expected_text, second_hypothesis);
        if reference_words > 0 {
            oracle_reference_words += reference_words;
            oracle_edits += first_edits.min(second_edits);
        }
        if first_normalized != second_normalized {
            disagreements += 1;
            if reference_words > 0 {
                disagreement_reference_words += reference_words;
                first_disagreement_edits += first_edits;
                second_disagreement_edits += second_edits;
            }
        }
    }

    PairAggregate {
        first_engine: first.engine.clone(),
        second_engine: second.engine.clone(),
        clips_compared: compared,
        normalized_agreement_rate: (compared > 0).then(|| agreed as f64 / compared as f64),
        disagreement_clips: disagreements,
        first_disagreement_wer: (disagreement_reference_words > 0)
            .then(|| first_disagreement_edits as f64 / disagreement_reference_words as f64),
        second_disagreement_wer: (disagreement_reference_words > 0)
            .then(|| second_disagreement_edits as f64 / disagreement_reference_words as f64),
        oracle_wer: (oracle_reference_words > 0)
            .then(|| oracle_edits as f64 / oracle_reference_words as f64),
    }
}

fn score_runs(
    manifest: &CorpusManifest,
    runs: &[EvaluationRun],
) -> Result<AggregateReport, String> {
    validate_manifest(manifest)?;
    if runs.is_empty() {
        return Err("score needs at least one evaluation run".to_string());
    }
    let mut run_ids = std::collections::HashSet::new();
    for run in runs {
        if !run_ids.insert(&run.run_id) {
            return Err(format!("duplicate run ID {:?}", run.run_id));
        }
        validate_run(manifest, run)?;
    }
    let engines = runs
        .iter()
        .map(|run| aggregate_engine(manifest, run))
        .collect();
    let mut pairs = Vec::new();
    for first in 0..runs.len() {
        for second in first + 1..runs.len() {
            pairs.push(aggregate_pair(manifest, &runs[first], &runs[second]));
        }
    }
    Ok(AggregateReport {
        schema_version: REPORT_SCHEMA_VERSION,
        corpus_name: manifest.name.clone(),
        clip_count: manifest.clips.len(),
        engines,
        pairs,
    })
}

fn validate_manifest(manifest: &CorpusManifest) -> Result<(), String> {
    if manifest.schema_version != CORPUS_SCHEMA_VERSION {
        return Err(format!(
            "unsupported corpus schema version {}; expected {CORPUS_SCHEMA_VERSION}",
            manifest.schema_version
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err("manifest name must not be empty".to_string());
    }
    if manifest.clips.is_empty() {
        return Err("manifest must contain at least one clip".to_string());
    }

    let mut ids = std::collections::HashSet::new();
    let mut wav_paths = std::collections::HashSet::new();
    for clip in &manifest.clips {
        if clip.id.is_empty()
            || !clip
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(format!(
                "clip ID {:?} must contain only ASCII letters, numbers, '-' or '_'",
                clip.id
            ));
        }
        if !ids.insert(&clip.id) {
            return Err(format!("duplicate clip ID {:?}", clip.id));
        }
        if clip.category.trim().is_empty() || clip.recording_condition.trim().is_empty() {
            return Err(format!(
                "clip {} needs a category and recording condition",
                clip.id
            ));
        }
        let wav_path = &clip.wav_path;
        if wav_path.is_absolute()
            || wav_path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
            || wav_path
                .extension()
                .and_then(|extension| extension.to_str())
                != Some("wav")
        {
            return Err(format!(
                "clip {} WAV path must be a relative .wav path inside the research directory",
                clip.id
            ));
        }
        if !wav_paths.insert(wav_path) {
            return Err(format!("duplicate WAV path {}", wav_path.display()));
        }
        if clip.proper_terms.iter().any(|term| term.trim().is_empty()) {
            return Err(format!("clip {} has an empty proper term", clip.id));
        }
    }
    Ok(())
}

fn write_f32_wav(path: &Path, samples: &[f32]) -> Result<(), String> {
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err("captured audio contains a non-finite sample".to_string());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let data_len = u32::try_from(samples.len().saturating_mul(4))
        .map_err(|_| "captured audio is too large for a WAV file".to_string())?;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36u32 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&16_000u32.to_le_bytes());
    bytes.extend_from_slice(&(16_000u32 * 4).to_le_bytes());
    bytes.extend_from_slice(&4u16.to_le_bytes());
    bytes.extend_from_slice(&32u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    std::fs::write(path, bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn read_validated_f32_wav(path: &Path) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()));
    }

    let mut offset = 12usize;
    let mut format_valid = false;
    while offset.checked_add(8).is_some_and(|end| end <= bytes.len()) {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(
            bytes[offset + 4..offset + 8]
                .try_into()
                .expect("four-byte slice"),
        ) as usize;
        let body_start = offset + 8;
        let body_end = body_start
            .checked_add(chunk_size)
            .ok_or_else(|| format!("{} has an invalid WAV chunk length", path.display()))?;
        if body_end > bytes.len() {
            return Err(format!("{} has a truncated WAV chunk", path.display()));
        }
        let body = &bytes[body_start..body_end];

        match chunk_id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err(format!(
                        "{} has a truncated WAV format chunk",
                        path.display()
                    ));
                }
                let format_tag = u16::from_le_bytes(body[0..2].try_into().unwrap());
                let channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
                let sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                if format_tag != 3 || channels != 1 || sample_rate != 16_000 || bits != 32 {
                    return Err(format!(
                        "{} must be mono 16 kHz IEEE float32 WAV (found format={format_tag}, channels={channels}, rate={sample_rate}, bits={bits})",
                        path.display()
                    ));
                }
                format_valid = true;
            }
            b"data" => {
                if !format_valid {
                    return Err(format!(
                        "{} has audio data before its format",
                        path.display()
                    ));
                }
                if body.len() % 4 != 0 {
                    return Err(format!("{} has a partial float32 sample", path.display()));
                }
                let samples: Vec<f32> = body
                    .chunks_exact(4)
                    .map(|sample| f32::from_le_bytes(sample.try_into().unwrap()))
                    .collect();
                if samples.iter().any(|sample| !sample.is_finite()) {
                    return Err(format!("{} contains a non-finite sample", path.display()));
                }
                return Ok(samples);
            }
            _ => {}
        }
        offset = body_end + (chunk_size & 1);
    }
    Err(format!("{} has no audio data chunk", path.display()))
}

fn next_missing_clip<'a>(
    research_dir: &Path,
    manifest: &'a CorpusManifest,
) -> Result<Option<&'a ClipSpec>, String> {
    for clip in &manifest.clips {
        let path = ensure_path_inside(research_dir, &clip.wav_path)?;
        if !path.exists() {
            return Ok(Some(clip));
        }
        read_validated_f32_wav(&path)?;
    }
    Ok(None)
}

fn save_recording(
    research_dir: &Path,
    clip: &ClipSpec,
    samples: &[f32],
    replace: bool,
) -> Result<(), String> {
    let path = ensure_path_inside(research_dir, &clip.wav_path)?;
    if path.exists() && !replace {
        return Err(format!(
            "clip {} is already recorded; choose re-record explicitly to replace it",
            clip.id
        ));
    }
    let temporary = path.with_extension(format!("wav.part-{}", std::process::id()));
    write_f32_wav(&temporary, samples)?;
    read_validated_f32_wav(&temporary)?;
    std::fs::rename(&temporary, &path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("could not save {}: {error}", path.display())
    })
}

fn delete_recording(research_dir: &Path, clip: &ClipSpec) -> Result<(), String> {
    let path = ensure_path_inside(research_dir, &clip.wav_path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("could not delete {}: {error}", path.display())),
    }
}

const MANIFEST_FILENAME: &str = "corpus.json";

const USAGE: &str = r#"Development-only local ASR research rig

Usage:
  asr_research init --research-dir <absolute-dir> --manifest <external-plan.json>
  asr_research record --research-dir <dir> [--clip <id>] [--replace]
  asr_research delete --research-dir <dir> --clip <id>
  asr_research validate --research-dir <dir>
  asr_research run --research-dir <dir> --run-id <id> --adapter <executable>
                   [--adapter-arg <argument>]...
  asr_research run-whisper --research-dir <dir> --run-id <id> --model <ggml.bin>
                           [--model-id <identity>] [--revision <identity>]
  asr_research score --research-dir <dir> --run <id> [--run <id>]...

Adapter process mode (normally started by `run-whisper`):
  asr_research whisper-adapter --model <ggml.bin>
                               [--model-id <identity>] [--revision <identity>]

Every corpus path is explicit. Corpus audio, reference text, and hypotheses stay
inside that directory. Only the `score` command writes aggregate JSON to stdout.
"#;

#[derive(Default)]
struct CliOptions {
    values: HashMap<String, Vec<String>>,
    switches: std::collections::HashSet<String>,
}

impl CliOptions {
    fn parse(arguments: impl Iterator<Item = String>, switches: &[&str]) -> Result<Self, String> {
        let switches: std::collections::HashSet<&str> = switches.iter().copied().collect();
        let mut arguments: VecDeque<String> = arguments.collect();
        let mut parsed = Self::default();
        while let Some(flag) = arguments.pop_front() {
            if !flag.starts_with("--") {
                return Err(format!("unexpected argument {flag:?}\n\n{USAGE}"));
            }
            if switches.contains(flag.as_str()) {
                parsed.switches.insert(flag);
                continue;
            }
            let value = arguments
                .pop_front()
                .ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))?;
            if value.starts_with("--") && flag != "--adapter-arg" {
                return Err(format!("{flag} needs a value\n\n{USAGE}"));
            }
            parsed.values.entry(flag).or_default().push(value);
        }
        Ok(parsed)
    }

    fn one(&self, flag: &str) -> Result<&str, String> {
        match self.values.get(flag).map(Vec::as_slice) {
            Some([value]) => Ok(value),
            Some(_) => Err(format!("pass {flag} exactly once")),
            None => Err(format!("{flag} is required\n\n{USAGE}")),
        }
    }

    fn optional(&self, flag: &str) -> Result<Option<&str>, String> {
        match self.values.get(flag).map(Vec::as_slice) {
            Some([value]) => Ok(Some(value)),
            Some(_) => Err(format!("pass {flag} at most once")),
            None => Ok(None),
        }
    }

    fn many(&self, flag: &str) -> &[String] {
        self.values.get(flag).map(Vec::as_slice).unwrap_or(&[])
    }

    fn reject_unknown(
        &self,
        allowed_values: &[&str],
        allowed_switches: &[&str],
    ) -> Result<(), String> {
        for flag in self.values.keys() {
            if !allowed_values.contains(&flag.as_str()) {
                return Err(format!("unknown option {flag}\n\n{USAGE}"));
            }
        }
        for flag in &self.switches {
            if !allowed_switches.contains(&flag.as_str()) {
                return Err(format!("unknown option {flag}\n\n{USAGE}"));
            }
        }
        Ok(())
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a repository parent")
        .to_path_buf()
}

fn slugtale_app_data_dirs() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        directories.push(home.join("Library/Application Support/com.slugtale.desktop"));
        directories.push(home.join("Library/Application Support/com.slugtale.app"));
        directories.push(home.join("Library/Application Support/Slugtale"));
        directories.push(home.join(".local/share/com.slugtale.desktop"));
        directories.push(home.join(".local/share/slugtale"));
    }
    if let Some(xdg_data) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        directories.push(xdg_data.join("com.slugtale.desktop"));
        directories.push(xdg_data.join("slugtale"));
    }
    if let Some(app_data) = std::env::var_os("APPDATA").map(PathBuf::from) {
        directories.push(app_data.join("com.slugtale.desktop"));
        directories.push(app_data.join("Slugtale"));
    }
    directories
}

fn validate_standard_research_dir(research_dir: &Path) -> Result<(), String> {
    validate_research_dir(research_dir, &repository_root(), &slugtale_app_data_dirs())
}

fn ensure_path_inside(research_dir: &Path, relative: &Path) -> Result<PathBuf, String> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "{} is not a safe relative research path",
            relative.display()
        ));
    }
    let root = normalized_absolute(research_dir)?;
    let candidate = root.join(relative);
    let resolved = normalized_absolute(&candidate)?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "{} escapes the research directory through a symlink",
            relative.display()
        ));
    }
    if !resolved.exists() {
        let parent = resolved
            .parent()
            .ok_or_else(|| format!("{} has no parent", resolved.display()))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    Ok(resolved)
}

fn load_manifest(research_dir: &Path) -> Result<CorpusManifest, String> {
    validate_standard_research_dir(research_dir)?;
    let path = ensure_path_inside(research_dir, Path::new(MANIFEST_FILENAME))?;
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let manifest: CorpusManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn find_clip<'a>(manifest: &'a CorpusManifest, clip_id: &str) -> Result<&'a ClipSpec, String> {
    manifest
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .ok_or_else(|| format!("manifest has no clip with ID {clip_id:?}"))
}

fn init_corpus(research_dir: &Path, manifest_source: &Path) -> Result<(), String> {
    validate_standard_research_dir(research_dir)?;
    let source_parent = manifest_source
        .parent()
        .ok_or_else(|| "manifest source needs a parent directory".to_string())?;
    validate_standard_research_dir(source_parent)?;
    if research_dir.exists()
        && research_dir
            .read_dir()
            .map_err(|error| error.to_string())?
            .next()
            .is_some()
    {
        return Err(format!(
            "{} already exists and is not empty",
            research_dir.display()
        ));
    }
    let manifest_bytes = std::fs::read(manifest_source)
        .map_err(|error| format!("could not read {}: {error}", manifest_source.display()))?;
    let manifest: CorpusManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("could not parse {}: {error}", manifest_source.display()))?;
    validate_manifest(&manifest)?;

    std::fs::create_dir_all(research_dir)
        .map_err(|error| format!("could not create {}: {error}", research_dir.display()))?;
    let manifest_path = ensure_path_inside(research_dir, Path::new(MANIFEST_FILENAME))?;
    let serialized = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("could not serialize manifest: {error}"))?;
    std::fs::write(&manifest_path, serialized)
        .map_err(|error| format!("could not write {}: {error}", manifest_path.display()))?;
    std::fs::create_dir_all(ensure_path_inside(research_dir, Path::new("clips"))?)
        .map_err(|error| format!("could not create clips directory: {error}"))?;
    std::fs::create_dir_all(ensure_path_inside(research_dir, Path::new("runs"))?)
        .map_err(|error| format!("could not create runs directory: {error}"))?;
    println!(
        "Created local research corpus at {} ({} clips).",
        research_dir.display(),
        manifest.clips.len()
    );
    Ok(())
}

fn read_line() -> Result<String, String> {
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|error| format!("could not read terminal input: {error}"))?;
    Ok(line.trim().to_string())
}

fn record_clip(research_dir: &Path, clip: &ClipSpec, replace: bool) -> Result<(), String> {
    let destination = ensure_path_inside(research_dir, &clip.wav_path)?;
    if destination.exists() && !replace {
        return Err(format!(
            "clip {} is already recorded; pass --replace to re-record it",
            clip.id
        ));
    }
    println!(
        "Clip {} · {} · {}",
        clip.id, clip.category, clip.recording_condition
    );
    println!("Reference: {}", clip.expected_text);
    println!("Press Enter to start recording, or type q then Enter to leave it for resume.");
    if read_line()?.eq_ignore_ascii_case("q") {
        return Ok(());
    }

    let mut recorder = CpalAudioRecorder::new();
    loop {
        recorder.start().map_err(|error| error.to_string())?;
        println!("Recording through Slugtale's microphone path. Press Enter to stop.");
        read_line()?;
        let audio = recorder.stop().map_err(|error| error.to_string())?;
        if audio.sample_rate_hz != 16_000 {
            return Err(format!(
                "capture returned {} Hz instead of 16 kHz",
                audio.sample_rate_hz
            ));
        }
        let seconds = audio.samples.len() as f64 / 16_000.0;
        let peak = audio
            .samples
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        println!("Captured {seconds:.2}s (peak {peak:.3}). [k]eep, [r]etry, [d]elete existing clip, or [q]uit?");
        match read_line()?.to_ascii_lowercase().as_str() {
            "" | "k" | "keep" => {
                save_recording(research_dir, clip, &audio.samples, replace)?;
                println!("Saved and validated {}.", clip.id);
                return Ok(());
            }
            "r" | "retry" => continue,
            "d" | "delete" => {
                delete_recording(research_dir, clip)?;
                println!("Deleted {}.", clip.id);
                return Ok(());
            }
            "q" | "quit" => return Ok(()),
            other => println!("Unknown choice {other:?}; the capture was not saved. Retrying."),
        }
    }
}

fn validate_corpus_audio(
    research_dir: &Path,
    manifest: &CorpusManifest,
    require_complete: bool,
) -> Result<usize, String> {
    let mut valid = 0usize;
    let mut missing = Vec::new();
    for clip in &manifest.clips {
        let path = ensure_path_inside(research_dir, &clip.wav_path)?;
        if path.exists() {
            read_validated_f32_wav(&path)?;
            valid += 1;
        } else {
            missing.push(clip.id.as_str());
        }
    }
    if require_complete && !missing.is_empty() {
        return Err(format!(
            "corpus is incomplete: {} clip(s) are not recorded ({})",
            missing.len(),
            missing.join(", ")
        ));
    }
    println!(
        "Manifest valid. Recordings: {valid} valid, {} missing.",
        missing.len()
    );
    Ok(valid)
}

fn run_adapter_process(
    command: &[String],
    requests: &[AdapterRequest],
) -> Result<Vec<AdapterResult>, String> {
    let (program, arguments) = command
        .split_first()
        .ok_or_else(|| "adapter command is empty".to_string())?;
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Adapter stderr is intentionally suppressed: an adapter must return
        // non-content errors in its schema, never leak a hypothesis via logs.
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start adapter {program:?}: {error}"))?;
    let mut request_bytes = Vec::new();
    for request in requests {
        serde_json::to_writer(&mut request_bytes, request)
            .map_err(|error| format!("could not encode adapter request: {error}"))?;
        request_bytes.push(b'\n');
    }
    let mut stdin = child.stdin.take().expect("adapter stdin is piped");
    let writer = std::thread::Builder::new()
        .name("slugtale-asr-adapter-input".to_string())
        .spawn(move || {
            stdin.write_all(&request_bytes)?;
            stdin.flush()
        })
        .map_err(|error| format!("could not start adapter input writer: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("could not wait for adapter: {error}"))?;
    writer
        .join()
        .map_err(|_| "adapter input writer panicked".to_string())?
        .map_err(|error| format!("could not write adapter request: {error}"))?;
    if !output.status.success() {
        return Err(format!("adapter exited with status {}", output.status));
    }
    let mut results = Vec::new();
    for line in output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let result: AdapterResult = serde_json::from_slice(line)
            .map_err(|error| format!("adapter returned invalid JSON: {error}"))?;
        results.push(result);
    }
    if results.len() != requests.len() {
        return Err(format!(
            "adapter returned {} results for {} requests",
            results.len(),
            requests.len()
        ));
    }
    for (request, result) in requests.iter().zip(&results) {
        if result.schema_version != ADAPTER_SCHEMA_VERSION || result.clip_id != request.clip_id {
            return Err(format!(
                "adapter returned a mismatched result for clip {}",
                request.clip_id
            ));
        }
    }
    Ok(results)
}

fn run_evaluation(
    research_dir: &Path,
    manifest: &CorpusManifest,
    run_id: &str,
    command: &[String],
) -> Result<(), String> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err("run ID must contain only ASCII letters, numbers, '-' or '_'".to_string());
    }
    validate_corpus_audio(research_dir, manifest, true)?;
    let requests: Vec<AdapterRequest> = manifest
        .clips
        .iter()
        .map(|clip| {
            Ok(AdapterRequest {
                schema_version: ADAPTER_SCHEMA_VERSION,
                clip_id: clip.id.clone(),
                wav_path: ensure_path_inside(research_dir, &clip.wav_path)?,
            })
        })
        .collect::<Result<_, String>>()?;
    let results = run_adapter_process(command, &requests)?;
    let engine = results
        .first()
        .ok_or_else(|| "adapter returned no results".to_string())?
        .engine
        .clone();
    if results.iter().any(|result| result.engine != engine) {
        return Err("adapter changed engine identity during the run".to_string());
    }
    let run = EvaluationRun {
        schema_version: RUN_SCHEMA_VERSION,
        run_id: run_id.to_string(),
        engine,
        results,
    };
    validate_run(manifest, &run)?;
    let relative = PathBuf::from("runs").join(format!("{run_id}.json"));
    let path = ensure_path_inside(research_dir, &relative)?;
    if path.exists() {
        return Err(format!(
            "run {run_id:?} already exists; choose a new run ID or delete it explicitly"
        ));
    }
    let serialized = serde_json::to_vec_pretty(&run)
        .map_err(|error| format!("could not serialize run: {error}"))?;
    std::fs::write(&path, serialized)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    let errors = run
        .results
        .iter()
        .filter(|result| result.error.is_some())
        .count();
    println!(
        "Stored sensitive local run {run_id:?} for {}/{} in {} ({} non-content errors).",
        run.engine.engine,
        run.engine.model,
        path.display(),
        errors
    );
    Ok(())
}

fn whisper_adapter(options: &CliOptions) -> Result<(), String> {
    options.reject_unknown(&["--model", "--model-id", "--revision"], &[])?;
    let model_path = PathBuf::from(options.one("--model")?);
    let identity = EngineIdentity {
        engine: "whisper".to_string(),
        model: options
            .optional("--model-id")?
            .unwrap_or("base.en")
            .to_string(),
        revision: options.optional("--revision")?.map(str::to_string),
    };
    let runtime = LocalWhisperRuntime::new(model_path);
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|error| format!("could not read adapter request: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let request: AdapterRequest = serde_json::from_str(&line)
            .map_err(|error| format!("could not parse adapter request: {error}"))?;
        if request.schema_version != ADAPTER_SCHEMA_VERSION {
            return Err(format!(
                "unsupported adapter request schema {}",
                request.schema_version
            ));
        }
        let started = std::time::Instant::now();
        let response = match read_validated_f32_wav(&request.wav_path) {
            Ok(samples) => {
                let audio_ms = samples.len() as f64 / 16.0;
                match runtime.transcribe(CapturedAudio::mono_16khz(samples)) {
                    Ok(transcription) => AdapterResult {
                        schema_version: ADAPTER_SCHEMA_VERSION,
                        clip_id: request.clip_id,
                        engine: identity.clone(),
                        hypothesis: Some(transcription.text),
                        confidence: None,
                        latency_ms: started.elapsed().as_secs_f64() * 1_000.0,
                        timings_ms: BTreeMap::from([("audio_duration".to_string(), audio_ms)]),
                        error: None,
                    },
                    Err(error) => AdapterResult {
                        schema_version: ADAPTER_SCHEMA_VERSION,
                        clip_id: request.clip_id,
                        engine: identity.clone(),
                        hypothesis: None,
                        confidence: None,
                        latency_ms: started.elapsed().as_secs_f64() * 1_000.0,
                        timings_ms: BTreeMap::new(),
                        error: Some(AdapterError {
                            code: "transcription_failed".to_string(),
                            detail: error.to_string(),
                        }),
                    },
                }
            }
            Err(error) => AdapterResult {
                schema_version: ADAPTER_SCHEMA_VERSION,
                clip_id: request.clip_id,
                engine: identity.clone(),
                hypothesis: None,
                confidence: None,
                latency_ms: started.elapsed().as_secs_f64() * 1_000.0,
                timings_ms: BTreeMap::new(),
                error: Some(AdapterError {
                    code: "invalid_audio".to_string(),
                    detail: error,
                }),
            },
        };
        serde_json::to_writer(&mut stdout, &response)
            .map_err(|error| format!("could not write adapter result: {error}"))?;
        stdout.write_all(b"\n").map_err(|error| error.to_string())?;
        stdout.flush().map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn load_run(research_dir: &Path, run_id: &str) -> Result<EvaluationRun, String> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(format!("unsafe run ID {run_id:?}"));
    }
    let path = ensure_path_inside(
        research_dir,
        &PathBuf::from("runs").join(format!("{run_id}.json")),
    )?;
    let bytes = std::fs::read(&path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))
}

fn run_main() -> Result<(), String> {
    let mut arguments = std::env::args();
    let _program = arguments.next();
    let command = arguments.next().ok_or_else(|| USAGE.to_string())?;
    let switches = if command == "record" {
        vec!["--replace"]
    } else {
        vec![]
    };
    let options = CliOptions::parse(arguments, &switches)?;
    match command.as_str() {
        "init" => {
            options.reject_unknown(&["--research-dir", "--manifest"], &[])?;
            init_corpus(
                Path::new(options.one("--research-dir")?),
                Path::new(options.one("--manifest")?),
            )
        }
        "record" => {
            options.reject_unknown(&["--research-dir", "--clip"], &["--replace"])?;
            let research_dir = Path::new(options.one("--research-dir")?);
            let manifest = load_manifest(research_dir)?;
            let clip = match options.optional("--clip")? {
                Some(clip_id) => find_clip(&manifest, clip_id)?,
                None => next_missing_clip(research_dir, &manifest)?.ok_or_else(|| {
                    "all clips are recorded; pass --clip <id> --replace to re-record one"
                        .to_string()
                })?,
            };
            record_clip(research_dir, clip, options.switches.contains("--replace"))
        }
        "delete" => {
            options.reject_unknown(&["--research-dir", "--clip"], &[])?;
            let research_dir = Path::new(options.one("--research-dir")?);
            let manifest = load_manifest(research_dir)?;
            let clip = find_clip(&manifest, options.one("--clip")?)?;
            delete_recording(research_dir, clip)?;
            println!(
                "Deleted recording {}. Its manifest entry remains for resume.",
                clip.id
            );
            Ok(())
        }
        "validate" => {
            options.reject_unknown(&["--research-dir"], &[])?;
            let research_dir = Path::new(options.one("--research-dir")?);
            let manifest = load_manifest(research_dir)?;
            validate_corpus_audio(research_dir, &manifest, false).map(|_| ())
        }
        "run" => {
            options.reject_unknown(
                &["--research-dir", "--run-id", "--adapter", "--adapter-arg"],
                &[],
            )?;
            let research_dir = Path::new(options.one("--research-dir")?);
            let manifest = load_manifest(research_dir)?;
            let mut adapter = vec![options.one("--adapter")?.to_string()];
            adapter.extend(options.many("--adapter-arg").iter().cloned());
            run_evaluation(research_dir, &manifest, options.one("--run-id")?, &adapter)
        }
        "run-whisper" => {
            options.reject_unknown(
                &[
                    "--research-dir",
                    "--run-id",
                    "--model",
                    "--model-id",
                    "--revision",
                ],
                &[],
            )?;
            if !cfg!(feature = "local-whisper-runtime") {
                return Err(
                    "run-whisper requires rebuilding this example with --features local-whisper-runtime"
                        .to_string(),
                );
            }
            let research_dir = Path::new(options.one("--research-dir")?);
            let manifest = load_manifest(research_dir)?;
            let executable = std::env::current_exe()
                .map_err(|error| format!("could not locate this executable: {error}"))?;
            let mut adapter = vec![
                executable.to_string_lossy().into_owned(),
                "whisper-adapter".to_string(),
                "--model".to_string(),
                options.one("--model")?.to_string(),
            ];
            if let Some(model_id) = options.optional("--model-id")? {
                adapter.extend(["--model-id".to_string(), model_id.to_string()]);
            }
            if let Some(revision) = options.optional("--revision")? {
                adapter.extend(["--revision".to_string(), revision.to_string()]);
            }
            run_evaluation(research_dir, &manifest, options.one("--run-id")?, &adapter)
        }
        "whisper-adapter" => whisper_adapter(&options),
        "score" => {
            options.reject_unknown(&["--research-dir", "--run"], &[])?;
            let research_dir = Path::new(options.one("--research-dir")?);
            let manifest = load_manifest(research_dir)?;
            let run_ids = options.many("--run");
            if run_ids.is_empty() {
                return Err("score needs at least one --run <id>".to_string());
            }
            let runs: Vec<EvaluationRun> = run_ids
                .iter()
                .map(|run_id| load_run(research_dir, run_id))
                .collect::<Result<_, _>>()?;
            let report = score_runs(&manifest, &runs)?;
            serde_json::to_writer_pretty(std::io::stdout().lock(), &report)
                .map_err(|error| format!("could not write aggregate report: {error}"))?;
            println!();
            Ok(())
        }
        _ => Err(format!("unknown command {command:?}\n\n{USAGE}")),
    }
}

fn main() {
    if let Err(error) = run_main() {
        eprintln!("asr_research: {error}");
        std::process::exit(2);
    }
}

fn normalized_absolute(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "choose an absolute research-data path, got {}",
            path.display()
        ));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut existing = normalized.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(metadata) => {
                let canonical = existing.canonicalize().map_err(|error| {
                    let kind = if metadata.file_type().is_symlink() {
                        "symlink"
                    } else {
                        "path"
                    };
                    format!("could not resolve {kind} {}: {error}", existing.display())
                })?;
                return Ok(missing
                    .iter()
                    .rev()
                    .fold(canonical, |path, component| path.join(component)));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or_else(|| {
                    format!(
                        "could not resolve any existing ancestor of {}",
                        path.display()
                    )
                })?;
                missing.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    format!(
                        "could not resolve any existing ancestor of {}",
                        path.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(format!("could not inspect {}: {error}", existing.display()));
            }
        }
    }
}

fn validate_research_dir(
    research_dir: &Path,
    repository: &Path,
    app_data_dirs: &[PathBuf],
) -> Result<(), String> {
    let research_dir = normalized_absolute(research_dir)?;
    let repository = normalized_absolute(repository)?;
    if research_dir == repository || research_dir.starts_with(&repository) {
        return Err("research data must not be stored in the Slugtale repository".to_string());
    }

    for app_data_dir in app_data_dirs {
        let app_data_dir = normalized_absolute(app_data_dir)?;
        if research_dir == app_data_dir || research_dir.starts_with(&app_data_dir) {
            return Err(
                "research data must not be stored in Slugtale application data or history"
                    .to_string(),
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> CorpusManifest {
        CorpusManifest {
            schema_version: CORPUS_SCHEMA_VERSION,
            name: "small local test".to_string(),
            clips: vec![ClipSpec {
                id: "001-command-clean".to_string(),
                expected_text: "Open the Slugtale settings.".to_string(),
                category: "short-command".to_string(),
                recording_condition: "clean".to_string(),
                wav_path: PathBuf::from("clips/001-command-clean.wav"),
                proper_terms: vec!["Slugtale".to_string()],
            }],
        }
    }

    #[test]
    fn research_data_must_stay_outside_the_repository_and_app_data() {
        let repository = PathBuf::from("/work/slugtale");
        let app_data =
            PathBuf::from("/Users/test/Library/Application Support/com.slugtale.desktop");

        assert!(validate_research_dir(&repository, &repository, &[app_data.clone()]).is_err());
        assert!(validate_research_dir(
            &repository.join("corpus"),
            &repository,
            &[app_data.clone()]
        )
        .is_err());
        assert!(
            validate_research_dir(&app_data.join("research"), &repository, &[app_data]).is_err()
        );
        assert!(
            validate_research_dir(Path::new("/Users/test/asr-research"), &repository, &[]).is_ok()
        );
    }

    #[cfg(unix)]
    #[test]
    fn research_paths_cannot_escape_through_symlinks() {
        use std::os::unix::fs::symlink;

        let base = std::env::temp_dir().join(format!(
            "slugtale-asr-research-symlink-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let repository = base.join("repository");
        let safe_root = base.join("safe-research");
        std::fs::create_dir_all(&repository).unwrap();
        std::fs::create_dir_all(safe_root.join("clips")).unwrap();

        let repository_alias = base.join("repository-alias");
        symlink(&repository, &repository_alias).unwrap();
        assert!(
            validate_research_dir(&repository_alias.join("corpus"), &repository, &[])
                .unwrap_err()
                .contains("repository")
        );

        let dangling = safe_root.join("clips/escaped.wav");
        symlink(base.join("outside/escaped.wav"), &dangling).unwrap();
        assert!(ensure_path_inside(&safe_root, Path::new("clips/escaped.wav")).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn manifest_rejects_duplicate_ids_and_escaping_wav_paths() {
        let mut duplicate = manifest();
        duplicate.clips.push(duplicate.clips[0].clone());
        assert!(validate_manifest(&duplicate)
            .unwrap_err()
            .contains("duplicate"));

        let mut escaping = manifest();
        escaping.clips[0].wav_path = PathBuf::from("../outside.wav");
        assert!(validate_manifest(&escaping)
            .unwrap_err()
            .contains("relative"));
    }

    #[test]
    fn wav_round_trip_is_strictly_mono_16khz_float32() {
        let dir = std::env::temp_dir().join(format!(
            "slugtale-asr-research-wav-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.wav");
        let samples = vec![0.0, 0.25, -0.5, 1.0];

        write_f32_wav(&path, &samples).unwrap();
        assert_eq!(read_validated_f32_wav(&path).unwrap(), samples);

        let mut wrong_rate = std::fs::read(&path).unwrap();
        wrong_rate[24..28].copy_from_slice(&48_000u32.to_le_bytes());
        std::fs::write(&path, wrong_rate).unwrap();
        assert!(read_validated_f32_wav(&path)
            .unwrap_err()
            .contains("16 kHz"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recording_session_resumes_and_rerecords_without_changing_the_manifest() {
        let dir = std::env::temp_dir().join(format!(
            "slugtale-asr-research-resume-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut corpus = manifest();
        corpus.clips.push(ClipSpec {
            id: "002-dictation-noisy".to_string(),
            expected_text: "Schedule it for Tuesday.".to_string(),
            category: "long-dictation".to_string(),
            recording_condition: "keyboard-noise".to_string(),
            wav_path: PathBuf::from("clips/002-dictation-noisy.wav"),
            proper_terms: vec![],
        });

        assert_eq!(
            next_missing_clip(&dir, &corpus).unwrap().unwrap().id,
            "001-command-clean"
        );
        save_recording(&dir, &corpus.clips[0], &[0.1, 0.2], false).unwrap();
        assert_eq!(
            next_missing_clip(&dir, &corpus).unwrap().unwrap().id,
            "002-dictation-noisy"
        );
        assert!(save_recording(&dir, &corpus.clips[0], &[0.3], false).is_err());
        save_recording(&dir, &corpus.clips[0], &[0.3], true).unwrap();
        assert_eq!(
            read_validated_f32_wav(&dir.join(&corpus.clips[0].wav_path)).unwrap(),
            vec![0.3]
        );
        delete_recording(&dir, &corpus.clips[0]).unwrap();
        assert_eq!(
            next_missing_clip(&dir, &corpus).unwrap().unwrap().id,
            "001-command-clean"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn result(
        clip_id: &str,
        engine: &EngineIdentity,
        hypothesis: &str,
        confidence: f64,
        latency_ms: f64,
    ) -> AdapterResult {
        AdapterResult {
            schema_version: ADAPTER_SCHEMA_VERSION,
            clip_id: clip_id.to_string(),
            engine: engine.clone(),
            hypothesis: Some(hypothesis.to_string()),
            confidence: Some(confidence),
            latency_ms,
            timings_ms: std::collections::BTreeMap::new(),
            error: None,
        }
    }

    #[test]
    fn scorer_reports_deterministic_engine_and_pair_aggregates() {
        let mut corpus = manifest();
        corpus.clips.push(ClipSpec {
            id: "002-silence".to_string(),
            expected_text: String::new(),
            category: "silence".to_string(),
            recording_condition: "clean".to_string(),
            wav_path: PathBuf::from("clips/002-silence.wav"),
            proper_terms: vec![],
        });
        corpus.clips.push(ClipSpec {
            id: "003-numbers".to_string(),
            expected_text: "Book two rooms.".to_string(),
            category: "numbers".to_string(),
            recording_condition: "clean".to_string(),
            wav_path: PathBuf::from("clips/003-numbers.wav"),
            proper_terms: vec![],
        });
        let first = EngineIdentity {
            engine: "first".into(),
            model: "a".into(),
            revision: None,
        };
        let second = EngineIdentity {
            engine: "second".into(),
            model: "b".into(),
            revision: Some("1".into()),
        };
        let runs = vec![
            EvaluationRun {
                schema_version: RUN_SCHEMA_VERSION,
                run_id: "first-run".into(),
                engine: first.clone(),
                results: vec![
                    result(
                        "001-command-clean",
                        &first,
                        "Open the Slugtale settings.",
                        0.9,
                        100.0,
                    ),
                    result("002-silence", &first, "hello", 0.8, 300.0),
                    result("003-numbers", &first, "Book three rooms", 0.4, 200.0),
                ],
            },
            EvaluationRun {
                schema_version: RUN_SCHEMA_VERSION,
                run_id: "second-run".into(),
                engine: second.clone(),
                results: vec![
                    result(
                        "001-command-clean",
                        &second,
                        "Open the slug tail settings",
                        0.6,
                        80.0,
                    ),
                    result("002-silence", &second, "", 0.9, 120.0),
                    result("003-numbers", &second, "Book two rooms.", 0.95, 100.0),
                ],
            },
        ];

        let report = score_runs(&corpus, &runs).unwrap();
        assert_eq!(report.engines.len(), 2);
        let a = &report.engines[0];
        assert!((a.normalized_wer.unwrap() - (1.0 / 7.0)).abs() < 1e-9);
        assert_eq!(a.proper_term_recall, Some(1.0));
        assert_eq!(a.silence_hallucination_rate, Some(1.0));
        assert_eq!(a.latency_p50_ms, Some(200.0));
        assert_eq!(a.latency_p95_ms, Some(300.0));

        let pair = &report.pairs[0];
        assert_eq!(pair.normalized_agreement_rate, Some(0.0));
        assert_eq!(pair.disagreement_clips, 3);
        assert_eq!(pair.oracle_wer, Some(0.0));
    }

    #[test]
    fn formatting_sequences_keep_marks_and_capitals_attached_to_word_positions() {
        let reference_punctuation = punctuation_sequence("Hello, world?");
        let moved_punctuation = punctuation_sequence("Hello world,?");
        assert_ne!(reference_punctuation, moved_punctuation);
        assert!(edit_distance(&reference_punctuation, &moved_punctuation) > 0);

        assert_ne!(
            capitalization_sequence("Slugtale meets Alice"),
            capitalization_sequence("slugtale meets Alice")
        );
    }
}
