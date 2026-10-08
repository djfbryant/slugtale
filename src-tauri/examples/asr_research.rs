//! Development-only local ASR corpus recorder and evaluation rig.

use slugtale_lib::{
    CapturedAudio, CpalAudioRecorder, DictationRecorder, LocalWhisperRuntime, SpeedProfile,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

const CORPUS_SCHEMA_VERSION: u32 = 1;
const ADAPTER_SCHEMA_VERSION: u32 = 1;
const RUN_SCHEMA_VERSION: u32 = 1;
const REPORT_SCHEMA_VERSION: u32 = 1;

/// The smallest number of human-voice clips the parent benchmark asks for.
///
/// Below this, a word error rate moves more when one clip is added or dropped
/// than when a model genuinely improves, so a smaller corpus publishes
/// pipeline measurements and no accuracy claim at all. The count is of human
/// clips, not clips in total: padding a corpus to this size with synthesized or
/// unattributed audio leaves the voice result exactly as small as it was.
const MINIMUM_BENCHMARK_CLIPS: usize = 100;

/// Where a clip's audio came from.
///
/// This is the field that keeps a report honest. A synthesized voice and a
/// person speaking into a microphone produce very different error rates for the
/// same text, so a corpus that does not say which one it holds cannot support a
/// claim about how dictation sounds to the user it is for.
///
/// `Unknown` is the default rather than a validation error so a manifest written
/// before this field existed keeps working. It is reported as unknown and never
/// counted as human: "we do not know" has to stay distinguishable from "it was
/// a person".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ClipProvenance {
    /// Recorded from a person speaking into the microphone, through Slugtale's
    /// own capture path. Only this can back a claim about the user's accuracy.
    Human,
    /// Synthesized speech from a text-to-speech voice. Useful for exercising
    /// capture, decode and scoring end to end; it is not a stand-in for a voice.
    Synthetic,
    /// The manifest did not say. Legacy corpora land here, and so does any clip
    /// the maintainer forgot to annotate.
    #[default]
    Unknown,
}

/// A condition a clip deliberately includes because it stresses something the
/// Dictation Workflow actually does.
///
/// Silence and names need no marker: an empty reference already identifies a
/// non-speech clip, and `proper_terms` already identifies a named one. Noise and
/// the segment edges are invisible from the audio alone — nothing in the
/// manifest can distinguish "quiet room" from "quiet room with a fan" — so a
/// clip has to say that it covers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum CoverageProbe {
    /// Speech with a competing sound in the room: fan, keyboard, traffic.
    Noise,
    /// Silence before the first word, so a segment starts on a real pause rather
    /// than on the artificial edge of a file.
    LeadingSegmentEdge,
    /// Silence after the last word, so a segment ends on the same pause the
    /// Segment Pause Detector acts on.
    TrailingSegmentEdge,
    /// A final sound quieter than the Dictation Bar's level threshold — the case
    /// where a level threshold, rather than a speech detector, decides whether a
    /// word made it into the segment.
    QuietSegmentEnding,
}

impl CoverageProbe {
    fn is_segment_edge(self) -> bool {
        matches!(
            self,
            CoverageProbe::LeadingSegmentEdge
                | CoverageProbe::TrailingSegmentEdge
                | CoverageProbe::QuietSegmentEnding
        )
    }
}

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
    /// Whether this clip is a human voice, a synthesized voice, or unattributed.
    /// A manifest that omits it loads as `Unknown`; see [`ClipProvenance`].
    #[serde(default)]
    provenance: ClipProvenance,
    /// A local pseudonym for the person who spoke the clip, used only to count
    /// distinct voices. Never a real name: the manifest is a file the maintainer
    /// may share or archive, and a real name in it is a privacy problem rather
    /// than a measurement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker: Option<String>,
    /// Conditions this clip covers on purpose. See [`CoverageProbe`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    probes: Vec<CoverageProbe>,
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

/// One measured dimension of the corpus, and whether it is present in enough
/// material to make a claim about.
///
/// A dimension nobody recorded is the most likely way a report overstates
/// itself, because an absent measurement looks exactly like a measurement
/// nobody looked for. So each dimension is always present, with the reason it
/// cannot be used when it is short.
#[derive(Debug, Clone, serde::Serialize)]
struct DimensionCoverage {
    dimension: String,
    clips: usize,
    supported: bool,
    reason: String,
}

/// What the corpus contains, and therefore what a report built from it may be
/// used to claim.
///
/// Every number here is a count or an aggregate label. No clip ID, reference
/// text, path or hypothesis reaches this struct, so it stays publishable
/// alongside the rest of the report.
#[derive(Debug, Clone, serde::Serialize)]
struct CorpusCoverage {
    clips_total: usize,
    clips_human: usize,
    clips_synthetic: usize,
    clips_unknown: usize,
    distinct_human_speakers: usize,
    named_clips: usize,
    distinct_proper_terms: usize,
    non_speech_clips: usize,
    noise_clips: usize,
    segment_edge_clips: usize,
    /// See [`MINIMUM_BENCHMARK_CLIPS`].
    minimum_benchmark_clips: usize,
    /// Whether this corpus can support any claim about dictation accuracy on a
    /// human voice: at least [`MINIMUM_BENCHMARK_CLIPS`] clips must be `human`
    /// on their own. Every whole-corpus accuracy field stays null unless this
    /// is true, so a report cannot present synthetic or unattributed audio as a
    /// voice result, and a corpus padded to the benchmark size with that audio
    /// does not open it.
    real_voice_accuracy_supported: bool,
    /// Everything that narrows what this report may be claimed to show, in
    /// order. A closed `real_voice_accuracy_supported` gate always appears
    /// here; a short dimension can appear while the gate is still open.
    gaps: Vec<String>,
    dimensions: Vec<DimensionCoverage>,
}

/// The same measurements restricted to one kind of audio.
///
/// A whole-corpus rate that mixes a person's voice with synthesized clips is a
/// statement about neither, which is why the slices exist: the label is what
/// makes a number readable, and the number is only meaningful next to it.
#[derive(Debug, Clone, serde::Serialize)]
struct ProvenanceSlice {
    provenance: ClipProvenance,
    clips_total: usize,
    clips_scored: usize,
    normalized_wer: Option<f64>,
    proper_term_recall: Option<f64>,
    silence_hallucination_rate: Option<f64>,
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
    /// Whether every clip in the corpus produced a hypothesis from this engine.
    /// A rate computed over the clips that happened to succeed is a rate over a
    /// corpus nobody chose, so this has to be visible next to the rate.
    clips_complete: bool,
    /// The accuracy measurements split by where the audio came from. Always
    /// contains all three kinds, so an absent one is visible as a zero.
    provenance_slices: Vec<ProvenanceSlice>,
    /// Non-content reasons the whole-corpus accuracy fields above are absent.
    /// Empty exactly when they are present.
    measurement_gaps: Vec<String>,
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
    /// Non-content reasons the pair accuracy fields above are absent. Empty
    /// exactly when they are present. A pair rate needs the same boundary as an
    /// engine rate — a corpus that can support a voice claim, no other audio
    /// mixed in — plus both engines transcribing every clip, because a rate
    /// over the pairs that happened to succeed describes a corpus nobody chose.
    measurement_gaps: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct AggregateReport {
    schema_version: u32,
    corpus_name: String,
    clip_count: usize,
    /// One plain sentence, first in the report, saying what this corpus can and
    /// cannot be used to claim. The structured gate is `coverage`; this is the
    /// line a reader meets before any number, so it cannot be skimmed past.
    claim_scope: String,
    coverage: CorpusCoverage,
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

/// Everything the scorer accumulates for one engine over some set of clips.
///
/// Grouping by provenance is just this struct built once per group, which keeps
/// the whole-corpus numbers and the per-provenance slices provably the same
/// measurement rather than two implementations that drift apart.
#[derive(Debug, Clone, Default)]
struct ScoreTotals {
    clips: usize,
    scored: usize,
    word_edits: usize,
    reference_words: usize,
    term_present: usize,
    term_recalled: usize,
    punctuation_edits: usize,
    punctuation_total: usize,
    capitalization_edits: usize,
    capitalization_total: usize,
    silence_clips: usize,
    silence_hallucinations: usize,
    latencies: Vec<f64>,
    confidences: Vec<(f64, bool)>,
}

impl ScoreTotals {
    fn add(&mut self, clip: &ClipSpec, result: &AdapterResult) {
        self.clips += 1;
        let Some(hypothesis) = result.hypothesis.as_deref() else {
            return;
        };
        self.scored += 1;
        self.latencies.push(result.latency_ms);
        let reference_normalized = normalize_for_wer(&clip.expected_text);
        let hypothesis_normalized = normalize_for_wer(hypothesis);
        if reference_normalized.is_empty() {
            self.silence_clips += 1;
            self.silence_hallucinations += usize::from(!hypothesis_normalized.is_empty());
        } else {
            let (edits, words) = word_errors(&clip.expected_text, hypothesis);
            self.word_edits += edits;
            self.reference_words += words;

            let reference_punctuation = punctuation_sequence(&clip.expected_text);
            let hypothesis_punctuation = punctuation_sequence(hypothesis);
            self.punctuation_edits += edit_distance(&reference_punctuation, &hypothesis_punctuation);
            self.punctuation_total += reference_punctuation
                .len()
                .max(hypothesis_punctuation.len());

            let reference_capitalization = capitalization_sequence(&clip.expected_text);
            let hypothesis_capitalization = capitalization_sequence(hypothesis);
            self.capitalization_edits +=
                edit_distance(&reference_capitalization, &hypothesis_capitalization);
            self.capitalization_total += reference_capitalization
                .len()
                .max(hypothesis_capitalization.len());
        }
        for term in &clip.proper_terms {
            if contains_normalized_phrase(&clip.expected_text, term) {
                self.term_present += 1;
                self.term_recalled += usize::from(contains_normalized_phrase(hypothesis, term));
            }
        }
        if let Some(confidence) = result.confidence {
            self.confidences
                .push((confidence, reference_normalized == hypothesis_normalized));
        }
    }

    fn normalized_wer(&self) -> Option<f64> {
        (self.reference_words > 0).then(|| self.word_edits as f64 / self.reference_words as f64)
    }

    fn proper_term_recall(&self) -> Option<f64> {
        (self.term_present > 0).then(|| self.term_recalled as f64 / self.term_present as f64)
    }

    fn silence_hallucination_rate(&self) -> Option<f64> {
        (self.silence_clips > 0)
            .then(|| self.silence_hallucinations as f64 / self.silence_clips as f64)
    }

    fn completeness_accuracy(
        edits: usize,
        total: usize,
    ) -> Option<f64> {
        (total > 0).then(|| (1.0 - edits as f64 / total as f64).clamp(0.0, 1.0))
    }

    fn punctuation_accuracy(&self) -> Option<f64> {
        Self::completeness_accuracy(self.punctuation_edits, self.punctuation_total)
    }

    fn capitalization_accuracy(&self) -> Option<f64> {
        Self::completeness_accuracy(self.capitalization_edits, self.capitalization_total)
    }

    fn confidence_ece(&self) -> Option<f64> {
        confidence_ece(&self.confidences)
    }
}

/// The order slices are reported in, so two reports of the same corpus serialize
/// identically. Unknown is last because it is the absence of an answer.
const PROVENANCE_ORDER: [ClipProvenance; 3] = [
    ClipProvenance::Human,
    ClipProvenance::Synthetic,
    ClipProvenance::Unknown,
];

/// The shared start of every "this corpus is not one voice" caveat.
///
/// A whole-corpus rate over a corpus that mixes a person with synthesized or
/// unattributed audio is a statement about neither, so the engine aggregates
/// and the pair aggregates both have to say why they withhold it. The caller
/// appends what that means for its own fields.
fn mixed_audio_reason(human: usize, synthetic: usize, unknown: usize) -> Option<String> {
    (synthetic > 0 || unknown > 0).then(|| {
        format!(
            "corpus mixes {synthetic} synthetic and {unknown} unattributed clip(s) with {human} \
             human-voice clip(s), so a whole-corpus rate would describe neither"
        )
    })
}

/// Count what the corpus contains, and decide whether that is enough to talk
/// about a voice at all.
///
/// The counts are derived from the manifest rather than trusted from it: a
/// "silence" clip is one whose reference is empty, a "names" clip is one whose
/// reference actually contains a listed term. Deriving them means a mislabelled
/// clip cannot inflate its own dimension.
fn coverage_of(manifest: &CorpusManifest) -> CorpusCoverage {
    let mut human = 0usize;
    let mut synthetic = 0usize;
    let mut unknown = 0usize;
    let mut speakers: BTreeSet<&str> = BTreeSet::new();
    let mut terms: BTreeSet<String> = BTreeSet::new();
    let mut named_clips = 0usize;
    let mut non_speech_clips = 0usize;
    let mut noise_clips = 0usize;
    let mut segment_edge_clips = 0usize;

    for clip in &manifest.clips {
        match clip.provenance {
            ClipProvenance::Human => {
                human += 1;
                if let Some(speaker) = clip.speaker.as_deref().filter(|s| !s.is_empty()) {
                    speakers.insert(speaker);
                }
            }
            ClipProvenance::Synthetic => synthetic += 1,
            ClipProvenance::Unknown => unknown += 1,
        }
        if normalize_for_wer(&clip.expected_text).is_empty() {
            non_speech_clips += 1;
        }
        let present: Vec<&String> = clip
            .proper_terms
            .iter()
            .filter(|term| contains_normalized_phrase(&clip.expected_text, term))
            .collect();
        if !present.is_empty() {
            named_clips += 1;
        }
        terms.extend(
            present
                .into_iter()
                .map(|term| normalize_for_wer(term))
                .filter(|term| !term.is_empty()),
        );
        if clip.probes.contains(&CoverageProbe::Noise) {
            noise_clips += 1;
        }
        if clip.probes.iter().copied().any(CoverageProbe::is_segment_edge) {
            segment_edge_clips += 1;
        }
    }

    let dimensions = vec![
        DimensionCoverage {
            dimension: "human-voices".to_string(),
            clips: human,
            supported: human > 0 && !speakers.is_empty(),
            reason: if human == 0 {
                format!(
                    "no clip is marked human ({synthetic} synthetic, {unknown} unattributed), so \
                     nothing here describes a voice"
                )
            } else if speakers.is_empty() {
                format!(
                    "{human} human clip(s) carry no speaker label, so they cannot be checked for \
                     more than one voice"
                )
            } else {
                format!(
                    "{} human clip(s) from {} labelled voice(s)",
                    human,
                    speakers.len()
                )
            },
        },
        DimensionCoverage {
            dimension: "names".to_string(),
            clips: named_clips,
            supported: named_clips > 0 && !terms.is_empty(),
            reason: if named_clips == 0 {
                "no clip's reference text contains a listed proper term, so name recall is \
                 unmeasured"
                    .to_string()
            } else {
                format!(
                    "{named_clips} clip(s) reference {} proper term(s)",
                    terms.len()
                )
            },
        },
        DimensionCoverage {
            dimension: "non-speech".to_string(),
            clips: non_speech_clips,
            supported: non_speech_clips > 0,
            reason: if non_speech_clips == 0 {
                "no clip has an empty reference, so a hallucination rate cannot be measured"
                    .to_string()
            } else {
                format!(
                    "{non_speech_clips} clip(s) have an empty reference, so hallucination rate is \
                     measurable"
                )
            },
        },
        DimensionCoverage {
            dimension: "noise".to_string(),
            clips: noise_clips,
            supported: noise_clips > 0,
            reason: if noise_clips == 0 {
                "no clip is marked with the noise probe, so speech in a competing sound is \
                 unmeasured"
                    .to_string()
            } else {
                format!("{noise_clips} clip(s) are marked as recorded with competing room noise")
            },
        },
        DimensionCoverage {
            dimension: "segment-edges".to_string(),
            clips: segment_edge_clips,
            supported: segment_edge_clips > 0,
            reason: if segment_edge_clips == 0 {
                "no clip covers a leading edge, trailing edge or quiet ending, so where a segment \
                 starts and stops is unmeasured"
                    .to_string()
            } else {
                format!(
                    "{segment_edge_clips} clip(s) cover a leading edge, trailing edge or quiet \
                     ending"
                )
            },
        },
    ];

    // The claim is about a human voice, so the human clips themselves have to
    // meet the benchmark size. A corpus padded to 100 with synthesized or
    // unattributed clips is still a pipeline check.
    let real_voice_accuracy_supported = human >= MINIMUM_BENCHMARK_CLIPS;
    let mut gaps = Vec::new();
    if human == 0 {
        gaps.push(format!(
            "no human-voice clip ({synthetic} synthetic, {unknown} unattributed): a synthesized \
             voice measures the pipeline, not the accuracy anyone would experience"
        ));
    } else if human < MINIMUM_BENCHMARK_CLIPS {
        gaps.push(format!(
            "corpus has {human} human-voice clip(s); the parent benchmark asks for at least \
             {MINIMUM_BENCHMARK_CLIPS} human clips"
        ));
    }
    if manifest.clips.len() < MINIMUM_BENCHMARK_CLIPS {
        gaps.push(format!(
            "corpus has {} clip(s); the parent benchmark asks for at least {MINIMUM_BENCHMARK_CLIPS}",
            manifest.clips.len()
        ));
    }
    gaps.extend(
        dimensions
            .iter()
            .filter(|dimension| !dimension.supported)
            .map(|dimension| format!("{}: {}", dimension.dimension, dimension.reason)),
    );

    CorpusCoverage {
        clips_total: manifest.clips.len(),
        clips_human: human,
        clips_synthetic: synthetic,
        clips_unknown: unknown,
        distinct_human_speakers: speakers.len(),
        named_clips,
        distinct_proper_terms: terms.len(),
        non_speech_clips,
        noise_clips,
        segment_edge_clips,
        minimum_benchmark_clips: MINIMUM_BENCHMARK_CLIPS,
        real_voice_accuracy_supported,
        gaps,
        dimensions,
    }
}

/// The sentence a reader meets before any number in the report.
///
/// It says what the corpus measured and, when that is not a voice, says so
/// before the rates rather than in a footnote underneath them.
fn claim_scope_for(coverage: &CorpusCoverage) -> String {
    if coverage.real_voice_accuracy_supported {
        let qualified = coverage
            .dimensions
            .iter()
            .filter(|dimension| !dimension.supported)
            .map(|dimension| dimension.dimension.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let qualification = if qualified.is_empty() {
            String::new()
        } else {
            format!(" No result here covers {qualified}.")
        };
        if coverage.clips_human < coverage.clips_total {
            format!(
                "This corpus holds {} human-voice clip(s) out of {} across {} labelled voice(s); \
                 its {} synthetic and {} unattributed clip(s) are not voice results, so \
                 whole-corpus accuracy is withheld and only the per-provenance slices describe \
                 the audio they name.{}",
                coverage.clips_human,
                coverage.clips_total,
                coverage.distinct_human_speakers,
                coverage.clips_synthetic,
                coverage.clips_unknown,
                qualification
            )
        } else if qualified.is_empty() {
            format!(
                "This corpus has {} human-voice clip(s) across {} labelled voice(s), so \
                 whole-corpus accuracy describes dictation on that voice and is not a general \
                 speech-recognition result.",
                coverage.clips_human, coverage.distinct_human_speakers
            )
        } else {
            format!(
                "This corpus has {} human-voice clip(s) across {} labelled voice(s); \
                 whole-corpus accuracy describes that voice only, and no result here covers {}.",
                coverage.clips_human, coverage.distinct_human_speakers, qualified
            )
        }
    } else {
        let shortfalls = coverage
            .gaps
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "This corpus cannot support a claim about dictation accuracy on a voice ({shortfalls}). \
             Whole-corpus accuracy fields are null on purpose; the per-provenance slices describe \
             only the audio they name."
        )
    }
}

fn aggregate_engine(
    manifest: &CorpusManifest,
    run: &EvaluationRun,
    coverage: &CorpusCoverage,
) -> EngineAggregate {
    let by_id: std::collections::HashMap<&str, &AdapterResult> = run
        .results
        .iter()
        .map(|result| (result.clip_id.as_str(), result))
        .collect();
    let mut whole = ScoreTotals::default();
    for clip in &manifest.clips {
        whole.add(clip, by_id[clip.id.as_str()]);
    }

    let mut measurement_gaps = Vec::new();
    if !coverage.real_voice_accuracy_supported {
        measurement_gaps.push(format!(
            "corpus cannot support a real-voice accuracy claim: {}",
            coverage.gaps.join("; ")
        ));
    } else if let Some(reason) = mixed_audio_reason(
        coverage.clips_human,
        coverage.clips_synthetic,
        coverage.clips_unknown,
    ) {
        // The corpus has enough human clips to be a benchmark, but it also
        // holds audio that is not a voice. Publishing one rate over the blend
        // would read as a voice result; the slices are the labelled numbers.
        measurement_gaps.push(format!(
            "{reason}; the per-provenance slices describe the audio they name"
        ));
    }
    if whole.scored < whole.clips {
        measurement_gaps.push(format!(
            "{} of {} clip(s) produced no hypothesis, so a whole-corpus rate would describe only \
             the clips that happened to succeed",
            whole.clips - whole.scored,
            whole.clips
        ));
    }
    // An accuracy field is published only when there is nothing to caveat. The
    // alternative — publishing a rate over the successful subset next to an
    // error count — reads as a rate over the corpus.
    let publishable = measurement_gaps.is_empty();

    let provenance_slices = PROVENANCE_ORDER
        .into_iter()
        .map(|provenance| {
            let mut totals = ScoreTotals::default();
            for clip in manifest
                .clips
                .iter()
                .filter(|clip| clip.provenance == provenance)
            {
                totals.add(clip, by_id[clip.id.as_str()]);
            }
            // An unattributed clip cannot be attributed here either. Reporting a
            // rate for it would hand back a number indistinguishable from a
            // voice result at a glance, which is exactly what the label exists
            // to prevent; the per-clip hypotheses remain in the sensitive run
            // file for anyone who re-annotates the manifest.
            let attributable = provenance != ClipProvenance::Unknown;
            ProvenanceSlice {
                provenance,
                clips_total: totals.clips,
                clips_scored: totals.scored,
                normalized_wer: attributable.then(|| totals.normalized_wer()).flatten(),
                proper_term_recall: attributable
                    .then(|| totals.proper_term_recall())
                    .flatten(),
                silence_hallucination_rate: attributable
                    .then(|| totals.silence_hallucination_rate())
                    .flatten(),
            }
        })
        .collect();

    EngineAggregate {
        engine: run.engine.clone(),
        clips_total: whole.clips,
        clips_scored: whole.scored,
        errors: whole.clips - whole.scored,
        normalized_wer: publishable.then(|| whole.normalized_wer()).flatten(),
        proper_term_recall: publishable.then(|| whole.proper_term_recall()).flatten(),
        punctuation_accuracy: publishable.then(|| whole.punctuation_accuracy()).flatten(),
        capitalization_accuracy: publishable.then(|| whole.capitalization_accuracy()).flatten(),
        silence_hallucination_rate: publishable
            .then(|| whole.silence_hallucination_rate())
            .flatten(),
        // Latency is a property of the machine and the model, not of whose voice
        // was recorded, so it survives every caveat above.
        latency_p50_ms: percentile(&whole.latencies, 50.0),
        latency_p95_ms: percentile(&whole.latencies, 95.0),
        confidence_ece: publishable.then(|| whole.confidence_ece()).flatten(),
        clips_complete: whole.scored == whole.clips,
        provenance_slices,
        measurement_gaps,
    }
}

fn aggregate_pair(
    manifest: &CorpusManifest,
    first: &EvaluationRun,
    second: &EvaluationRun,
    coverage: &CorpusCoverage,
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

    // The same boundary as the engine aggregates. A pair rate over a corpus
    // that cannot support a voice claim, or that mixes other audio into it,
    // reads exactly like the voice result the boundary exists to prevent. An
    // incomplete pair is the survivor problem again: both engines have to have
    // transcribed every clip, or the rate describes the clips that happened to
    // succeed. Counts and latencies stay.
    let mut measurement_gaps = Vec::new();
    if !coverage.real_voice_accuracy_supported {
        measurement_gaps.push(format!(
            "corpus cannot support a real-voice accuracy claim: {}",
            coverage.gaps.join("; ")
        ));
    } else if let Some(reason) = mixed_audio_reason(
        coverage.clips_human,
        coverage.clips_synthetic,
        coverage.clips_unknown,
    ) {
        measurement_gaps.push(format!("{reason}; pair accuracy fields are withheld"));
    }
    if compared < manifest.clips.len() {
        measurement_gaps.push(format!(
            "{} of {} clip(s) did not produce a hypothesis from both engines, so a pair rate \
             would describe only the clips that happened to succeed",
            manifest.clips.len() - compared,
            manifest.clips.len()
        ));
    }
    let publishable = measurement_gaps.is_empty();

    PairAggregate {
        first_engine: first.engine.clone(),
        second_engine: second.engine.clone(),
        clips_compared: compared,
        normalized_agreement_rate: (publishable && compared > 0)
            .then(|| agreed as f64 / compared as f64),
        disagreement_clips: disagreements,
        first_disagreement_wer: (publishable && disagreement_reference_words > 0)
            .then(|| first_disagreement_edits as f64 / disagreement_reference_words as f64),
        second_disagreement_wer: (publishable && disagreement_reference_words > 0)
            .then(|| second_disagreement_edits as f64 / disagreement_reference_words as f64),
        oracle_wer: (publishable && oracle_reference_words > 0)
            .then(|| oracle_edits as f64 / oracle_reference_words as f64),
        measurement_gaps,
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
    let coverage = coverage_of(manifest);
    let engines = runs
        .iter()
        .map(|run| aggregate_engine(manifest, run, &coverage))
        .collect();
    let mut pairs = Vec::new();
    for first in 0..runs.len() {
        for second in first + 1..runs.len() {
            pairs.push(aggregate_pair(
                manifest,
                &runs[first],
                &runs[second],
                &coverage,
            ));
        }
    }
    Ok(AggregateReport {
        schema_version: REPORT_SCHEMA_VERSION,
        corpus_name: manifest.name.clone(),
        clip_count: manifest.clips.len(),
        claim_scope: claim_scope_for(&coverage),
        coverage,
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
        // A speaker label is a pseudonym for counting distinct voices, so it is
        // held to the same shape as a clip ID: a shape a real name, an email
        // address or a path would not survive.
        if let Some(speaker) = &clip.speaker {
            if speaker.is_empty()
                || !speaker
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            {
                return Err(format!(
                    "clip {} has a speaker label that is not a pseudonym; use a local label such as \
                     'voice-1', never a person's name",
                    clip.id
                ));
            }
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

Each clip carries the provenance of its audio ("human", "synthetic", or
"unknown"), an optional pseudonym speaker label, and any noise or segment-edge
probes it covers. A report says which of those the corpus actually holds and
leaves whole-corpus accuracy null unless at least 100 clips are human-voice on
their own and no other audio is mixed in; a legacy manifest without the fields
loads as unattributed rather than failing.
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
        "Clip {} · {} · {} · provenance {}",
        clip.id,
        clip.category,
        clip.recording_condition,
        match clip.provenance {
            ClipProvenance::Human => "human",
            ClipProvenance::Synthetic => "synthetic",
            ClipProvenance::Unknown => "unattributed",
        }
    );
    println!("Reference: {}", clip.expected_text);
    if clip.provenance == ClipProvenance::Unknown {
        println!(
            "This clip's provenance is unattributed, so it will not count as a voice result. If \
             you are recording it yourself, set \"provenance\": \"human\" and a \"speaker\" label \
             in the manifest."
        );
    }
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
    let coverage = coverage_of(manifest);
    println!(
        "Manifest valid. Recordings: {valid} valid, {} missing.",
        missing.len()
    );
    let claim_state = if !coverage.real_voice_accuracy_supported {
        "not supported"
    } else if coverage.clips_human < coverage.clips_total {
        "supported for the human clips; whole-corpus rates stay null while other audio is mixed in"
    } else {
        "supported"
    };
    println!(
        "Provenance: {} human, {} synthetic, {} unattributed. Real-voice accuracy claims: {}.",
        coverage.clips_human, coverage.clips_synthetic, coverage.clips_unknown, claim_state
    );
    if !coverage.real_voice_accuracy_supported {
        // Surfaced while the corpus is still being recorded, because the cheapest
        // moment to add a human clip or a speaker label is before the recordings
        // exist, not after a run has already been scored from them.
        for gap in &coverage.gaps {
            println!("  gap: {gap}");
        }
    }
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
    let runtime = LocalWhisperRuntime::new(slugtale_lib::LocalModelRef::at(model_path));
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
                // Balanced, the app's default. The rig measures transcription,
                // not decode strategies, and its manifest records no profile.
                match runtime
                    .transcribe(CapturedAudio::mono_16khz(samples), SpeedProfile::default())
                {
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
    use slugtale_lib::{
        audio_level_from_samples, captured_audio_from_interleaved_input, is_digital_silence,
        is_voice_level, voice_level_from_rms, SegmentPauseDetector, DEFAULT_SEGMENT_PAUSE_SECS,
        DIGITAL_SILENCE_EPSILON, VOICE_LEVEL,
    };
    use std::time::Duration;

    fn clip(id: &str, expected_text: &str) -> ClipSpec {
        ClipSpec {
            id: id.to_string(),
            expected_text: expected_text.to_string(),
            category: "short-command".to_string(),
            recording_condition: "clean".to_string(),
            wav_path: PathBuf::from(format!("clips/{id}.wav")),
            proper_terms: vec![],
            provenance: ClipProvenance::Human,
            speaker: None,
            probes: vec![],
        }
    }

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
                provenance: ClipProvenance::Human,
                speaker: None,
                probes: vec![],
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

    /// A corpus recorded before provenance existed is still a real corpus. It
    /// must keep working, and it must keep saying so: the honest answer to "we
    /// do not know what this audio is" is to report it as unknown, never to
    /// guess human.
    #[test]
    fn a_manifest_without_provenance_loads_as_unattributed_and_stays_that_way() {
        let legacy = br#"{
            "schema_version": 1,
            "name": "recorded before provenance",
            "clips": [
                {
                    "id": "001-command-clean",
                    "expected_text": "Open the Slugtale settings.",
                    "category": "short-command",
                    "recording_condition": "clean-close-microphone",
                    "wav_path": "clips/001-command-clean.wav",
                    "proper_terms": ["Slugtale"]
                }
            ]
        }"#;

        let parsed: CorpusManifest = serde_json::from_slice(legacy).expect("legacy manifest parses");
        assert_eq!(parsed.clips[0].provenance, ClipProvenance::Unknown);
        assert_eq!(parsed.clips[0].speaker, None);
        assert!(parsed.clips[0].probes.is_empty());
        assert!(validate_manifest(&parsed).is_ok());

        let coverage = coverage_of(&parsed);
        assert_eq!(coverage.clips_unknown, 1);
        assert_eq!(coverage.clips_human, 0);
        assert!(!coverage.real_voice_accuracy_supported);

        // `init` re-serializes the manifest into the research directory, so the
        // stored copy has to state the unknown rather than drop the field and
        // leave a later reader to assume it was human.
        let stored = serde_json::to_string(&parsed).unwrap();
        assert!(
            stored.contains("\"provenance\":\"unknown\""),
            "stored manifest must spell out the unknown: {stored}"
        );
    }

    /// Annotating provenance is only useful if the values are constrained. An
    /// unrecognized one has to fail loudly here, not become a fourth kind of
    /// audio that no count ever notices.
    #[test]
    fn provenance_and_speaker_labels_are_constrained() {
        let readable: CorpusManifest = serde_json::from_slice(
            br#"{
                "schema_version": 1,
                "name": "annotated",
                "clips": [
                    {
                        "id": "001-command-clean",
                        "expected_text": "Open the Slugtale settings.",
                        "category": "short-command",
                        "recording_condition": "clean-close-microphone",
                        "wav_path": "clips/001-command-clean.wav",
                        "provenance": "human",
                        "speaker": "voice-1",
                        "probes": ["noise", "quiet-segment-ending"]
                    }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(readable.clips[0].provenance, ClipProvenance::Human);
        assert_eq!(readable.clips[0].speaker.as_deref(), Some("voice-1"));
        assert_eq!(
            readable.clips[0].probes,
            vec![CoverageProbe::Noise, CoverageProbe::QuietSegmentEnding]
        );

        let misspelled = serde_json::from_slice::<CorpusManifest>(
            br#"{
                "schema_version": 1,
                "name": "misspelled",
                "clips": [
                    {
                        "id": "001-command-clean",
                        "expected_text": "Open the Slugtale settings.",
                        "category": "short-command",
                        "recording_condition": "clean",
                        "wav_path": "clips/001-command-clean.wav",
                        "provenance": "real-person"
                    }
                ]
            }"#,
        );
        assert!(misspelled.is_err());

        // The speaker label is held to a pseudonym's shape. A real name in the
        // manifest is a privacy problem in a file the maintainer may share, and
        // it is much cheaper to refuse it at the door.
        let mut named = manifest();
        named.clips[0].speaker = Some("Darren Bryant".to_string());
        assert!(validate_manifest(&named)
            .unwrap_err()
            .contains("pseudonym"));
        let mut blank = manifest();
        blank.clips[0].speaker = Some(String::new());
        assert!(validate_manifest(&blank).is_err());
    }

    /// A synthesized corpus is a legitimate thing to build — it exercises
    /// capture, decode and scoring end to end. It is not evidence about a voice,
    /// so the report has to withhold the whole-corpus rate rather than print it
    /// and ask the reader to remember the caveat.
    #[test]
    fn a_synthetic_or_unattributed_corpus_never_publishes_a_whole_corpus_rate() {
        let engine = EngineIdentity {
            engine: "whisper".into(),
            model: "base.en".into(),
            revision: None,
        };
        for (label, provenance) in [
            ("synthetic", ClipProvenance::Synthetic),
            ("unattributed", ClipProvenance::Unknown),
        ] {
            let mut corpus = manifest();
            corpus.name = format!("{label} smoke corpus");
            corpus.clips = (0..4)
                .map(|index| {
                    let mut spec = clip(&format!("00{index}-clip"), "Say hello now.");
                    spec.provenance = provenance;
                    spec
                })
                .collect();
            let run = EvaluationRun {
                schema_version: RUN_SCHEMA_VERSION,
                run_id: "run".into(),
                engine: engine.clone(),
                // Perfectly clean text: a synthesized corpus scores beautifully,
                // which is exactly the number that must not escape as accuracy.
                results: corpus
                    .clips
                    .iter()
                    .map(|spec| result(&spec.id, &engine, "Say hello now.", 0.95, 120.0))
                    .collect(),
            };

            let report = score_runs(&corpus, &[run]).unwrap();
            assert!(!report.coverage.real_voice_accuracy_supported, "{label}");
            assert!(
                report.claim_scope.contains("cannot support a claim"),
                "{label}: {}",
                report.claim_scope
            );
            let aggregate = &report.engines[0];
            assert_eq!(aggregate.normalized_wer, None, "{label}");
            assert_eq!(aggregate.punctuation_accuracy, None, "{label}");
            assert!(!aggregate.measurement_gaps.is_empty(), "{label}");
            // Latency is a property of the machine and the model, not of whose
            // voice was recorded, so it survives the caveat.
            assert_eq!(aggregate.latency_p50_ms, Some(120.0), "{label}");
        }
    }

    /// The other half of the integrity rule: a corpus that really is people
    /// really speaking, in the quantity the benchmark asks for, publishes its
    /// rates. A gate that can never open would be as useless as no gate.
    #[test]
    fn a_sufficient_human_corpus_does_publish_its_rates() {
        let engine = EngineIdentity {
            engine: "whisper".into(),
            model: "base.en".into(),
            revision: None,
        };
        let corpus = provenance_corpus(MINIMUM_BENCHMARK_CLIPS, 0, 0);
        let run = perfect_run(&corpus, &engine, "run");

        let report = score_runs(&corpus, &[run]).unwrap();
        assert!(report.coverage.real_voice_accuracy_supported);
        assert!(report.coverage.gaps.is_empty(), "{:?}", report.coverage.gaps);
        assert_eq!(report.coverage.distinct_human_speakers, 2);
        assert!(report
            .coverage
            .dimensions
            .iter()
            .all(|dimension| dimension.supported));
        let aggregate = &report.engines[0];
        assert_eq!(aggregate.normalized_wer, Some(0.0));
        assert_eq!(aggregate.proper_term_recall, Some(1.0));
        assert_eq!(aggregate.silence_hallucination_rate, Some(0.0));
        assert!(aggregate.clips_complete);
        assert!(aggregate.measurement_gaps.is_empty());
        let human = aggregate
            .provenance_slices
            .iter()
            .find(|slice| slice.provenance == ClipProvenance::Human)
            .unwrap();
        assert_eq!(human.normalized_wer, Some(0.0));
    }

    /// The benchmark gate counts human clips, not clips. A corpus padded to
    /// 100 with synthesized or unattributed audio is the exact shape that used
    /// to open the gate with a single human clip in it.
    #[test]
    fn the_human_benchmark_gate_counts_human_clips_not_corpus_size() {
        // 100 clips, one of them a voice.
        let padded = coverage_of(&provenance_corpus(1, 99, 0));
        assert_eq!(padded.clips_total, MINIMUM_BENCHMARK_CLIPS);
        assert_eq!(padded.clips_human, 1);
        assert!(!padded.real_voice_accuracy_supported);
        assert!(
            padded
                .gaps
                .iter()
                .any(|gap| gap.contains("1 human-voice clip(s)")),
            "{:?}",
            padded.gaps
        );

        // 100 clips, 99 of them voices: still one clip short.
        let almost = coverage_of(&provenance_corpus(99, 0, 1));
        assert_eq!(almost.clips_total, MINIMUM_BENCHMARK_CLIPS);
        assert!(!almost.real_voice_accuracy_supported);
        assert!(
            almost
                .gaps
                .iter()
                .any(|gap| gap.contains("99 human-voice clip(s)")),
            "{:?}",
            almost.gaps
        );

        // 100 human clips with no other audio: the benchmark itself.
        let sufficient = coverage_of(&provenance_corpus(MINIMUM_BENCHMARK_CLIPS, 0, 0));
        assert!(sufficient.real_voice_accuracy_supported);
        assert!(sufficient.gaps.is_empty(), "{:?}", sufficient.gaps);

        // 100 human clips plus synthetic audio: the human benchmark is still
        // there, but the corpus is no longer one voice.
        let mixed = coverage_of(&provenance_corpus(MINIMUM_BENCHMARK_CLIPS, 2, 0));
        assert!(mixed.real_voice_accuracy_supported);
        assert_eq!(mixed.clips_total, MINIMUM_BENCHMARK_CLIPS + 2);
        assert!(mixed.gaps.is_empty(), "{:?}", mixed.gaps);
    }

    /// A corpus big enough for the benchmark, but not all one voice, must not
    /// publish the blend as a voice result: the whole-corpus fields are
    /// withheld and the labelled slices carry the numbers.
    #[test]
    fn a_mixed_corpus_does_not_publish_its_whole_corpus_rate() {
        let engine = EngineIdentity {
            engine: "whisper".into(),
            model: "base.en".into(),
            revision: None,
        };
        let corpus = provenance_corpus(MINIMUM_BENCHMARK_CLIPS, 2, 0);
        let run = perfect_run(&corpus, &engine, "run");

        let report = score_runs(&corpus, &[run]).unwrap();
        assert!(report.coverage.real_voice_accuracy_supported);
        assert!(
            report.claim_scope.contains("out of 102"),
            "the claim must say how many clips are human rather than count the whole corpus as one \
             voice: {}",
            report.claim_scope
        );
        assert!(report.claim_scope.contains("whole-corpus accuracy is withheld"));

        let aggregate = &report.engines[0];
        assert_eq!(aggregate.normalized_wer, None);
        assert_eq!(aggregate.punctuation_accuracy, None);
        assert_eq!(aggregate.capitalization_accuracy, None);
        assert!(
            aggregate
                .measurement_gaps
                .iter()
                .any(|gap| gap.contains("mixes 2 synthetic")),
            "{:?}",
            aggregate.measurement_gaps
        );
        // Counts and latency are not voice claims, so they survive.
        assert_eq!(aggregate.clips_total, MINIMUM_BENCHMARK_CLIPS + 2);
        assert_eq!(aggregate.latency_p50_ms, Some(100.0));
        // The honest labelled slices remain.
        let slice = |provenance: ClipProvenance| {
            aggregate
                .provenance_slices
                .iter()
                .find(|slice| slice.provenance == provenance)
                .unwrap()
        };
        assert_eq!(
            slice(ClipProvenance::Human).clips_total,
            MINIMUM_BENCHMARK_CLIPS
        );
        assert_eq!(slice(ClipProvenance::Human).normalized_wer, Some(0.0));
        assert_eq!(slice(ClipProvenance::Synthetic).clips_total, 2);
        assert_eq!(slice(ClipProvenance::Synthetic).normalized_wer, Some(0.0));
    }

    /// A run where the engine failed on some clips used to publish a rate over
    /// the survivors with an error count printed underneath it. That rate
    /// describes a corpus nobody chose.
    #[test]
    fn an_incomplete_run_withholds_accuracy_instead_of_scoring_the_survivors() {
        let engine = EngineIdentity {
            engine: "whisper".into(),
            model: "base.en".into(),
            revision: None,
        };
        let mut corpus = manifest();
        corpus.name = "real voices".to_string();
        corpus.clips = (0..MINIMUM_BENCHMARK_CLIPS)
            .map(|index| {
                let mut spec = clip(&format!("clip-{index:04}"), "Open the Slugtale settings.");
                spec.speaker = Some("voice-1".to_string());
                spec.probes = vec![CoverageProbe::TrailingSegmentEdge];
                spec
            })
            .collect();
        let mut results: Vec<AdapterResult> = corpus
            .clips
            .iter()
            .map(|spec| result(spec.id.as_str(), &engine, &spec.expected_text, 0.95, 100.0))
            .collect();
        let failed = results.len() - 3;
        for outcome in results.iter_mut().take(failed) {
            outcome.hypothesis = None;
            outcome.confidence = None;
            outcome.error = Some(AdapterError {
                code: "transcription_failed".to_string(),
                detail: "local runtime failure".to_string(),
            });
        }
        let run = EvaluationRun {
            schema_version: RUN_SCHEMA_VERSION,
            run_id: "run".into(),
            engine,
            results,
        };

        let report = score_runs(&corpus, &[run]).unwrap();
        let aggregate = &report.engines[0];
        assert!(report.coverage.real_voice_accuracy_supported);
        assert!(!aggregate.clips_complete);
        assert_eq!(aggregate.normalized_wer, None);
        assert_eq!(
            aggregate.errors, failed,
            "the error count is still reported, it just no longer rides beside a rate"
        );
        assert!(
            aggregate
                .measurement_gaps
                .iter()
                .any(|gap| gap.contains("produced no hypothesis")),
            "{:?}",
            aggregate.measurement_gaps
        );
        // The per-provenance slice counts the failure too, so the shortfall is
        // visible at the same level as the number it qualifies.
        let human = aggregate
            .provenance_slices
            .iter()
            .find(|slice| slice.provenance == ClipProvenance::Human)
            .unwrap();
        assert_eq!(human.clips_total, MINIMUM_BENCHMARK_CLIPS);
        assert_eq!(human.clips_scored, MINIMUM_BENCHMARK_CLIPS - failed);
    }

    /// The pair rates sit behind the same boundary as the engine rates: a
    /// corpus padded to 100 clips with synthesized audio withholds them even
    /// though it is large enough to look like a benchmark.
    #[test]
    fn a_pair_on_a_padded_corpus_withholds_its_rates() {
        let first_engine = EngineIdentity {
            engine: "first".into(),
            model: "a".into(),
            revision: None,
        };
        let second_engine = EngineIdentity {
            engine: "second".into(),
            model: "b".into(),
            revision: None,
        };
        let corpus = provenance_corpus(1, 99, 0);
        let runs = vec![
            perfect_run(&corpus, &first_engine, "first-run"),
            perfect_run(&corpus, &second_engine, "second-run"),
        ];

        let report = score_runs(&corpus, &runs).unwrap();
        assert!(!report.coverage.real_voice_accuracy_supported);
        let pair = &report.pairs[0];
        assert_eq!(pair.clips_compared, MINIMUM_BENCHMARK_CLIPS);
        assert_eq!(pair.normalized_agreement_rate, None);
        assert_eq!(pair.first_disagreement_wer, None);
        assert_eq!(pair.second_disagreement_wer, None);
        assert_eq!(pair.oracle_wer, None);
        assert!(
            pair.measurement_gaps
                .iter()
                .any(|gap| gap.contains("real-voice accuracy claim")),
            "{:?}",
            pair.measurement_gaps
        );
    }

    /// A pair that covers every clip on a corpus that can support the claim
    /// publishes its rates, so the boundary does not swallow the measurements
    /// it exists to qualify.
    #[test]
    fn a_sufficient_pair_publishes_its_rates() {
        let first_engine = EngineIdentity {
            engine: "first".into(),
            model: "a".into(),
            revision: None,
        };
        let second_engine = EngineIdentity {
            engine: "second".into(),
            model: "b".into(),
            revision: None,
        };
        let corpus = provenance_corpus(MINIMUM_BENCHMARK_CLIPS, 0, 0);
        let first = perfect_run(&corpus, &first_engine, "first-run");
        let mut second = perfect_run(&corpus, &second_engine, "second-run");
        // One clip hears "setting" instead of "settings": one disagreement, so
        // the disagreement rates are measured rather than assumed.
        let differing = second
            .results
            .iter_mut()
            .find(|result| result.clip_id == "clip-0001")
            .unwrap();
        differing.hypothesis = Some("Open the Slugtale setting.".to_string());

        let report = score_runs(&corpus, &[first, second]).unwrap();
        let pair = &report.pairs[0];
        assert!(pair.measurement_gaps.is_empty(), "{:?}", pair.measurement_gaps);
        assert_eq!(pair.clips_compared, MINIMUM_BENCHMARK_CLIPS);
        assert_eq!(pair.disagreement_clips, 1);
        assert!((pair.normalized_agreement_rate.unwrap() - 0.99).abs() < 1e-9);
        assert_eq!(pair.first_disagreement_wer, Some(0.0));
        assert!((pair.second_disagreement_wer.unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(pair.oracle_wer, Some(0.0));
    }

    /// A pair rate over the clips both engines happened to transcribe is the
    /// survivor problem again: the rates are withheld and the counts stay.
    #[test]
    fn an_incomplete_pair_withholds_its_rates_instead_of_scoring_the_survivors() {
        let first_engine = EngineIdentity {
            engine: "first".into(),
            model: "a".into(),
            revision: None,
        };
        let second_engine = EngineIdentity {
            engine: "second".into(),
            model: "b".into(),
            revision: None,
        };
        let corpus = provenance_corpus(MINIMUM_BENCHMARK_CLIPS, 0, 0);
        let first = perfect_run(&corpus, &first_engine, "first-run");
        let mut second = perfect_run(&corpus, &second_engine, "second-run");
        let failed = second.results.len() - 3;
        for outcome in second.results.iter_mut().take(failed) {
            outcome.hypothesis = None;
            outcome.confidence = None;
            outcome.error = Some(AdapterError {
                code: "transcription_failed".to_string(),
                detail: "local runtime failure".to_string(),
            });
        }

        let report = score_runs(&corpus, &[first, second]).unwrap();
        // Completeness is a property of each run and of the pair that joins
        // them: the complete run still publishes its own rate.
        assert_eq!(report.engines[0].normalized_wer, Some(0.0));
        let pair = &report.pairs[0];
        assert_eq!(pair.clips_compared, 3);
        assert_eq!(pair.disagreement_clips, 0);
        assert_eq!(pair.normalized_agreement_rate, None);
        assert_eq!(pair.first_disagreement_wer, None);
        assert_eq!(pair.second_disagreement_wer, None);
        assert_eq!(pair.oracle_wer, None);
        assert!(
            pair.measurement_gaps
                .iter()
                .any(|gap| gap.contains("both engines")),
            "{:?}",
            pair.measurement_gaps
        );
    }

    /// The dimensions the finding names — voices, names, silence, noise and
    /// segment edges — have to be countable and, when absent, named.
    #[test]
    fn coverage_counts_every_dimension_and_names_the_ones_that_are_missing() {
        let mut corpus = manifest();
        corpus.clips = vec![
            ClipSpec {
                probes: vec![CoverageProbe::Noise],
                speaker: Some("voice-1".to_string()),
                proper_terms: vec!["Slugtale".to_string()],
                ..clip("001-noisy", "Open the Slugtale settings.")
            },
            ClipSpec {
                speaker: Some("voice-2".to_string()),
                proper_terms: vec!["Slugtale".to_string(), "Parakeet".to_string()],
                ..clip("002-second-voice", "Compare Slugtale and Parakeet.")
            },
            ClipSpec {
                // A proper term the reference never says must not inflate the
                // names dimension: the scorer only scores terms that occur.
                proper_terms: vec!["whisper.cpp".to_string()],
                ..clip("003-unused-term", "Open the settings.")
            },
            ClipSpec {
                probes: vec![CoverageProbe::TrailingSegmentEdge],
                ..clip("004-silence", "")
            },
            ClipSpec {
                probes: vec![
                    CoverageProbe::LeadingSegmentEdge,
                    CoverageProbe::QuietSegmentEnding,
                ],
                ..clip("005-quiet-ending", "Open the Slugtale settings.")
            },
        ];

        let coverage = coverage_of(&corpus);
        assert_eq!(coverage.clips_human, 5);
        assert_eq!(coverage.distinct_human_speakers, 2);
        assert_eq!(coverage.named_clips, 2);
        assert_eq!(coverage.distinct_proper_terms, 2);
        assert_eq!(coverage.non_speech_clips, 1);
        assert_eq!(coverage.noise_clips, 1);
        assert_eq!(coverage.segment_edge_clips, 2);

        let dimension = |name: &str| {
            coverage
                .dimensions
                .iter()
                .find(|dimension| dimension.dimension == name)
                .unwrap_or_else(|| panic!("{name} missing from coverage"))
        };
        assert_eq!(dimension("human-voices").clips, 5);
        assert_eq!(dimension("names").clips, 2);
        assert_eq!(dimension("non-speech").clips, 1);
        assert_eq!(dimension("noise").clips, 1);
        assert_eq!(dimension("segment-edges").clips, 2);
        assert!(coverage.dimensions.iter().all(|dimension| dimension.supported));

        // Strip the corpus back to the bare minimum and every dimension has to
        // report itself unsupported rather than quietly read as zero-or-fine.
        let mut bare = manifest();
        bare.clips = vec![clip("001-bare", "Open the settings.")];
        let bare_coverage = coverage_of(&bare);
        assert!(!bare_coverage.real_voice_accuracy_supported);
        for name in [
            "human-voices",
            "names",
            "non-speech",
            "noise",
            "segment-edges",
        ] {
            let unsupported = bare_coverage
                .dimensions
                .iter()
                .find(|dimension| dimension.dimension == name)
                .unwrap();
            assert!(!unsupported.supported, "{name} should be unsupported");
            assert!(
                bare_coverage
                    .gaps
                    .iter()
                    .any(|gap| gap.starts_with(name)),
                "{name} shortfall is missing from gaps: {:?}",
                bare_coverage.gaps
            );
        }
    }

    /// Provenance is what the whole-corpus gate reads, so a label that no
    /// dimension depends on is a label nothing enforces. This keeps the two
    /// agreeing: the scorer and the report must slice on the same field.
    #[test]
    fn the_report_slices_on_the_same_provenance_the_manifest_records() {
        let engine = EngineIdentity {
            engine: "whisper".into(),
            model: "base.en".into(),
            revision: None,
        };
        let mut corpus = manifest();
        corpus.clips = vec![
            clip("001-human", "Open the Slugtale settings."),
            ClipSpec {
                provenance: ClipProvenance::Synthetic,
                ..clip("002-synthetic", "Open the Slugtale settings.")
            },
            ClipSpec {
                provenance: ClipProvenance::Unknown,
                ..clip("003-unknown", "Open the Slugtale settings.")
            },
        ];
        let run = EvaluationRun {
            schema_version: RUN_SCHEMA_VERSION,
            run_id: "run".into(),
            engine: engine.clone(),
            results: corpus
                .clips
                .iter()
                // Only the human clip is transcribed correctly. If the slices
                // were computed over something other than `provenance`, the
                // synthetic slice could not come out as the worst one.
                .map(|spec| {
                    let hypothesis = if spec.provenance == ClipProvenance::Human {
                        spec.expected_text.clone()
                    } else {
                        "Open the slug tail settings.".to_string()
                    };
                    result(spec.id.as_str(), &engine, &hypothesis, 0.9, 100.0)
                })
                .collect(),
        };

        let report = score_runs(&corpus, &[run]).unwrap();
        let aggregate = &report.engines[0];
        assert_eq!(
            aggregate
                .provenance_slices
                .iter()
                .map(|slice| (slice.provenance, slice.clips_total, slice.normalized_wer))
                .collect::<Vec<_>>(),
            vec![
                (ClipProvenance::Human, 1, Some(0.0)),
                (ClipProvenance::Synthetic, 1, Some(0.5)),
                (ClipProvenance::Unknown, 1, None),
            ]
        );
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
            provenance: ClipProvenance::Human,
            speaker: None,
            probes: vec![CoverageProbe::Noise],
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

    /// A corpus split by provenance, sized to whatever boundary a test needs.
    ///
    /// Every clip carries a proper term, a probe and (for humans) a speaker
    /// label, so all five coverage dimensions are supported and a test about
    /// provenance is not accidentally also a test about a missing dimension.
    fn provenance_corpus(human: usize, synthetic: usize, unknown: usize) -> CorpusManifest {
        let mut corpus = manifest();
        corpus.name = "provenance boundary".to_string();
        corpus.clips = Vec::new();
        let mut index = 0usize;
        for (provenance, count) in [
            (ClipProvenance::Human, human),
            (ClipProvenance::Synthetic, synthetic),
            (ClipProvenance::Unknown, unknown),
        ] {
            for _ in 0..count {
                let mut spec = clip(&format!("clip-{index:04}"), "Open the Slugtale settings.");
                spec.provenance = provenance;
                spec.proper_terms = vec!["Slugtale".to_string()];
                if provenance == ClipProvenance::Human {
                    spec.speaker = Some(if index % 2 == 0 {
                        "voice-1".to_string()
                    } else {
                        "voice-2".to_string()
                    });
                }
                if index % 5 == 0 {
                    spec.expected_text = String::new();
                    spec.proper_terms.clear();
                    spec.probes = vec![CoverageProbe::Noise];
                } else {
                    spec.probes = vec![CoverageProbe::TrailingSegmentEdge];
                }
                corpus.clips.push(spec);
                index += 1;
            }
        }
        corpus
    }

    /// One run whose hypothesis is the reference text itself, so a test about
    /// gating is not also a test about error rates.
    fn perfect_run(
        corpus: &CorpusManifest,
        engine: &EngineIdentity,
        run_id: &str,
    ) -> EvaluationRun {
        EvaluationRun {
            schema_version: RUN_SCHEMA_VERSION,
            run_id: run_id.to_string(),
            engine: engine.clone(),
            results: corpus
                .clips
                .iter()
                .map(|spec| result(spec.id.as_str(), engine, &spec.expected_text, 0.95, 100.0))
                .collect(),
        }
    }

    #[test]
    fn scorer_reports_deterministic_engine_and_pair_aggregates() {
        let mut corpus = manifest();
        corpus.clips.push(clip("002-silence", ""));
        corpus.clips.push(clip("003-numbers", "Book two rooms."));
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
        // Three clips is a pipeline smoke test, not a benchmark, so the
        // whole-corpus accuracy fields are withheld. The measurements still
        // exist; they sit on the slice for the audio they actually describe.
        assert!(!report.coverage.real_voice_accuracy_supported);
        assert_eq!(a.normalized_wer, None);
        assert_eq!(a.latency_p50_ms, Some(200.0));
        assert_eq!(a.latency_p95_ms, Some(300.0));
        assert!(a.clips_complete);

        let human = a
            .provenance_slices
            .iter()
            .find(|slice| slice.provenance == ClipProvenance::Human)
            .unwrap();
        assert_eq!(human.clips_total, 3);
        assert!((human.normalized_wer.unwrap() - (1.0 / 7.0)).abs() < 1e-9);
        assert_eq!(human.proper_term_recall, Some(1.0));
        assert_eq!(human.silence_hallucination_rate, Some(1.0));

        let pair = &report.pairs[0];
        // The pair rates sit behind the same boundary as the engine rates: a
        // three-clip smoke corpus cannot support a voice claim, so agreement,
        // disagreement WER and oracle WER are withheld here too. The counts
        // still describe what was compared.
        assert_eq!(pair.normalized_agreement_rate, None);
        assert_eq!(pair.first_disagreement_wer, None);
        assert_eq!(pair.second_disagreement_wer, None);
        assert_eq!(pair.oracle_wer, None);
        assert_eq!(pair.clips_compared, 3);
        assert_eq!(pair.disagreement_clips, 3);
        assert!(
            pair.measurement_gaps
                .iter()
                .any(|gap| gap.contains("real-voice accuracy claim")),
            "{:?}",
            pair.measurement_gaps
        );
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

    /// One level tick, 20 ms, which is roughly the cadence the capture loop
    /// reports voice levels at. Segment-edge behaviour is a question about
    /// timescales, so the fixtures below are built in ticks rather than in
    /// seconds of real time.
    const LEVEL_WINDOW: usize = 320;
    const TICK_MS: u64 = 20;
    /// The Segment Pause the shipped Settings File defaults to. Production
    /// derives each dictation's pause from the Settings it pins (ADR-0026), so
    /// the shipped default is what these detector replays measure.
    const SEGMENT_PAUSE: Duration = Duration::from_secs(DEFAULT_SEGMENT_PAUSE_SECS as u64);

    /// A deterministic stand-in for random noise, so a fixture is a pure
    /// function of its parameters and no test depends on a seed or a clock.
    fn noise(seed: u32) -> impl Iterator<Item = f32> {
        let mut state = seed | 1;
        std::iter::from_fn(move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            Some((state as f32 / u32::MAX as f32) * 2.0 - 1.0)
        })
    }

    /// A microphone's noise floor: not digital silence, and far below speech.
    fn room_tone(seed: u32) -> Vec<f32> {
        noise(seed).take(16_000).map(|value| value * 0.003).collect()
    }

    /// A speech stand-in with an amplitude-modulated carrier, so it has the
    /// envelope a voice has instead of the flat top of a test tone.
    fn speech(seed: u32, amplitude: f32) -> Vec<f32> {
        noise(seed)
            .take(16_000)
            .enumerate()
            .map(|(index, noise_value)| {
                let t = index as f32 / 16_000.0;
                let carrier = (2.0 * std::f32::consts::PI * 180.0 * t).sin();
                let envelope = 0.5 + 0.5 * (2.0 * std::f32::consts::PI * 4.0 * t).sin();
                amplitude * (carrier + 0.05 * noise_value) * envelope
            })
            .collect()
    }

    /// A word at an even, known level. The level pipeline compares an RMS
    /// against a fixed threshold, so a fixture whose RMS does not depend on
    /// where a window happens to fall is what a boundary measurement needs.
    fn even_level_word(amplitude: f32) -> Vec<f32> {
        (0..16_000)
            .map(|index| {
                let t = index as f32 / 16_000.0;
                amplitude * (2.0 * std::f32::consts::PI * 180.0 * t).sin()
            })
            .collect()
    }

    /// A steady room hum, loud enough to matter and even in pitch. It contains
    /// no speech at all; the level pipeline has no way of knowing that.
    fn fan(seed: u32) -> Vec<f32> {
        noise(seed)
            .take(16_000)
            .enumerate()
            .map(|(index, noise_value)| {
                let t = index as f32 / 16_000.0;
                0.05 * (2.0 * std::f32::consts::PI * 120.0 * t).sin() + 0.004 * noise_value
            })
            .collect()
    }

    /// One representative window from the middle of a fixture, so a tick reads
    /// the steady part of the signal rather than an onset.
    fn window(samples: &[f32]) -> &[f32] {
        let start = samples.len() / 2 - LEVEL_WINDOW / 2;
        &samples[start..start + LEVEL_WINDOW]
    }

    /// The level the Dictation Bar renders and the Segment Pause Detector reads,
    /// built from the production helpers rather than a copy of them. A test that
    /// passes here is evidence about the shipped numbers.
    fn voice_level(samples: &[f32]) -> f32 {
        voice_level_from_rms(audio_level_from_samples(samples))
    }

    /// Replays a level timeline through the production detector and returns the
    /// ticks at which it asked for the segment to be flushed.
    fn flush_ticks(timeline: &[&[f32]]) -> Vec<usize> {
        let mut detector = SegmentPauseDetector::with_pause(SEGMENT_PAUSE);
        let start = std::time::Instant::now();
        let mut flushed = Vec::new();
        for (tick, samples) in timeline.iter().enumerate() {
            let at = start + Duration::from_millis(tick as u64 * TICK_MS);
            if detector.on_level(voice_level(samples), at) {
                flushed.push(tick);
            }
        }
        flushed
    }

    /// The finding this unit answers is that a level threshold is not a speech
    /// detector: a fan can look like speech, and a quiet final sound can fall
    /// below the limit. This is a measurement of where the shipped helpers draw
    /// that line. It deliberately claims nothing about whether a learned
    /// detector would draw it better — that has to be measured rather than
    /// assumed, and adding one is out of scope here (ADR-0027).
    #[test]
    fn signal_helpers_tell_digital_silence_room_tone_and_speech_apart() {
        let silence = vec![0.0f32; 16_000];
        let quiet_room = room_tone(7);
        let speech_level = speech(11, 0.35);
        let hum = fan(13);

        // A denied microphone supplies a correctly timed buffer of zeros, which
        // is a different thing from a quiet room, and the app has to keep them
        // apart: Whisper canonically reads a zero buffer as a word.
        assert!(is_digital_silence(&silence));
        assert!(!is_digital_silence(&quiet_room));
        assert!(!is_digital_silence(&speech_level));
        assert!(!is_digital_silence(&hum));
        let room_peak = quiet_room
            .iter()
            .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
        assert!(
            room_peak > DIGITAL_SILENCE_EPSILON,
            "a quiet room peaks at {room_peak}, above the {DIGITAL_SILENCE_EPSILON} silence \
             threshold, which is the whole reason that rule needs both an RMS and a peak term"
        );

        assert_eq!(audio_level_from_samples(&[]), 0.0);
        assert!(audio_level_from_samples(&quiet_room) < audio_level_from_samples(&speech_level));
        assert!(is_voice_level(voice_level(&speech_level)));
        assert!(!is_voice_level(voice_level(&quiet_room)));
        // The fan is the point of the whole exercise: no speech in it at all, and
        // unambiguously above the threshold the Segment Pause Detector uses.
        assert!(is_voice_level(voice_level(&hum)));
    }

    /// The capture path resamples and downmixes before anything else sees the
    /// audio, so a fault here is invisible in every downstream measurement. The
    /// comparison is against what naive point sampling would have done with the
    /// same stream, because that is the regression the box filter exists to
    /// prevent (slugtale-8dj).
    #[test]
    fn the_capture_resampler_preserves_the_speech_band_and_attenuates_above_it() {
        let source_rate = 48_000;
        let source_tone = |hz: f32| -> Vec<f32> {
            (0..source_rate)
                .map(|index| {
                    let t = index as f32 / source_rate as f32;
                    0.5 * (2.0 * std::f32::consts::PI * hz * t).sin()
                })
                .collect()
        };
        let interleave = |mono: &[f32]| -> Vec<f32> {
            mono.iter()
                .flat_map(|value| std::iter::repeat(*value).take(2))
                .collect()
        };
        let channel_mean = |interleaved: &[f32]| -> Vec<f32> {
            interleaved
                .chunks_exact(2)
                .map(|frame| frame.iter().copied().sum::<f32>() / 2.0)
                .collect()
        };

        let speech_band = source_tone(300.0);
        let interleaved = interleave(&speech_band);
        let resampled = captured_audio_from_interleaved_input(source_rate, 2, &interleaved).unwrap();
        assert_eq!(resampled.sample_rate_hz, 16_000);
        let expected = speech_band.len() / 3;
        assert!(
            (resampled.samples.len() as i64 - expected as i64).abs() <= 1,
            "3:1 decimation produced {} samples, expected about {expected}",
            resampled.samples.len()
        );
        // A resampler that flattens the voice is worse than no resampler, so
        // the speech band has to come through at its original amplitude.
        let before = audio_level_from_samples(&channel_mean(&interleaved));
        let after = audio_level_from_samples(&resampled.samples);
        assert!(
            (after / before - 1.0).abs() < 0.05,
            "300 Hz level moved from {before} to {after}"
        );
        // Mono must stay mono: a two-channel stream is one voice, not two.
        assert!(is_voice_level(voice_level(&resampled.samples)));

        // Above the 8 kHz Nyquist limit of the 16 kHz target, the box filter
        // attenuates rather than folding the tone back down into the speech
        // band. It attenuates, it does not remove, and recording that honestly
        // is the difference between a measurement and a quality claim.
        let above_nyquist = interleave(&source_tone(12_000.0));
        let filtered =
            captured_audio_from_interleaved_input(source_rate, 2, &above_nyquist).unwrap();
        let naive: Vec<f32> = channel_mean(&above_nyquist)
            .into_iter()
            .step_by(3)
            .collect();
        let naive_level = audio_level_from_samples(&naive);
        let filtered_level = audio_level_from_samples(&filtered.samples);
        assert!(
            (naive_level - before).abs() < 0.01,
            "point sampling keeps full amplitude ({naive_level} against {before}), so the two \
             paths are compared like for like"
        );
        assert!(
            filtered_level < 0.5 * naive_level,
            "a 12 kHz tone survived at {filtered_level} against {naive_level} for point sampling"
        );
        assert!(
            filtered_level > 0.0,
            "the box filter attenuates an out-of-band tone, it does not delete it"
        );
    }

    /// Segment edges are where a dictation actually breaks: a pause that is
    /// missed merges two sentences, and a pause that arrives early cuts a word
    /// off. Both are decided by the level threshold, so both are worth recording.
    #[test]
    fn the_level_threshold_ends_a_segment_on_a_real_pause_and_cannot_see_a_fan() {
        let speech_fixture = speech(21, 0.35);
        let quiet_fixture = room_tone(22);
        let fan_fixture = fan(23);
        let speech_window = window(&speech_fixture);
        let quiet_window = window(&quiet_fixture);
        let fan_window = window(&fan_fixture);

        // Two seconds of speech, then eight seconds of a genuine pause. The
        // five-second Segment Pause is the shipped default rather than a scaled
        // stand-in, so this exercises the real threshold.
        let mut timeline: Vec<&[f32]> = Vec::new();
        timeline.extend(std::iter::repeat(speech_window).take(100));
        timeline.extend(std::iter::repeat(quiet_window).take(400));
        let flushed = flush_ticks(&timeline);
        assert_eq!(
            flushed.len(),
            1,
            "one pause ends one segment, and the silence after it ends nothing"
        );
        let last_word = 99;
        let elapsed = Duration::from_millis(flushed[0] as u64 * TICK_MS)
            - Duration::from_millis(last_word as u64 * TICK_MS);
        assert!(
            elapsed >= SEGMENT_PAUSE && elapsed < SEGMENT_PAUSE + Duration::from_millis(2 * TICK_MS),
            "flushed {elapsed:?} after the last word, against a {SEGMENT_PAUSE:?} pause"
        );

        // The same speech and the same pause, but the room has a fan in it. The
        // threshold cannot tell a steady hum from a voice, so the pause never
        // elapses and the dictation never flushes. This is a measurement of the
        // shipped rule, and the reason a level threshold is not a speech
        // detector.
        let mut noisy: Vec<&[f32]> = Vec::new();
        noisy.extend(std::iter::repeat(speech_window).take(100));
        noisy.extend(std::iter::repeat(fan_window).take(400));
        assert!(
            is_voice_level(voice_level(fan_window)),
            "the fixture has to be above the threshold or the comparison means nothing"
        );
        assert!(
            flush_ticks(&noisy).is_empty(),
            "a sustained hum above the threshold holds every pause open indefinitely"
        );

        // The other end of the same rule, measured either side of the line. A
        // final word a little above the threshold reads as voice; a little below
        // it reads as silence, and the segment edge lands after that word
        // instead of on it. These numbers are where the shipped threshold falls,
        // not a claim that a different threshold would be better.
        let just_heard = even_level_word(0.0198);
        let just_missed = even_level_word(0.0180);
        assert!(is_voice_level(voice_level(&just_heard)));
        assert!(!is_voice_level(voice_level(&just_missed)));
        assert!(voice_level(window(&just_missed)) < VOICE_LEVEL);
        assert!(voice_level(window(&just_heard)) < 1.0);
    }
}
