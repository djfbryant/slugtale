//! The whisper.cpp bindings: every `whisper_rs` name in Slugtale, and nothing
//! else.
//!
//! whisper.cpp is a C++ library with a C ABI, so this module is the one place
//! that knows the raw types, the raw parameters, and the raw segment iterator.
//! Everything crossing back into the rest of `asr` is either a domain type
//! ([`FinalTranscription`]) or a plain value ([`RawSegment`]), which is what
//! lets [`crate::asr::whisper_runtime`] own the model's lifetime without
//! spelling a single binding, and lets the decode policy
//! ([`crate::asr::machine_probe`]) stay a pure function that can be tested
//! without a model file.
//!
//! Nothing in this module knows which caller is asking, or with what
//! Transcription Speed Profile: the decode settings arrive as a parameter.

#[cfg(feature = "local-whisper-runtime")]
use super::machine_probe::WhisperDecodeSettings;
#[cfg(feature = "local-whisper-runtime")]
use crate::AsrError;
use crate::{FinalTranscription, TranscriptSegment};

/// The loaded whisper.cpp model context. Owned by
/// [`crate::asr::whisper_runtime::LocalWhisperRuntime`], which decides when it
/// is created and when it is dropped; this module only borrows it.
#[cfg(feature = "local-whisper-runtime")]
pub(super) type WhisperContext = whisper_rs::WhisperContext;

/// Open a model file. Reading and parsing it is expensive, so the caller does
/// this once per model path and keeps the context.
#[cfg(feature = "local-whisper-runtime")]
pub(super) fn open_context(model_path: &str) -> Result<WhisperContext, AsrError> {
    whisper_rs::WhisperContext::new_with_params(
        model_path,
        whisper_rs::WhisperContextParameters::default(),
    )
    .map_err(|error| AsrError::Runtime(error.to_string()))
}

/// One recognized segment as whisper.cpp reports it: text plus centisecond
/// timestamps. Converted to a [`TranscriptSegment`] by
/// [`transcript_from_whisper_segments`] before it reaches the domain.
#[cfg(any(test, feature = "local-whisper-runtime"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RawSegment {
    pub(super) text: String,
    pub(super) start_cs: i64,
    pub(super) end_cs: i64,
}

/// Decode `samples` with an already-open context. The settings come from the
/// machine probe, so this module decides no policy of its own — it only carries
/// the profile's decision across the FFI.
#[cfg(feature = "local-whisper-runtime")]
pub(super) fn decode(
    context: &WhisperContext,
    samples: &[f32],
    settings: WhisperDecodeSettings,
) -> Result<Vec<RawSegment>, AsrError> {
    use super::machine_probe::WhisperDecodeStrategy;

    let mut state = context
        .create_state()
        .map_err(|error| AsrError::Runtime(error.to_string()))?;
    let mut params = whisper_rs::FullParams::new(match settings.strategy {
        WhisperDecodeStrategy::Greedy { best_of } => {
            whisper_rs::SamplingStrategy::Greedy { best_of }
        }
        WhisperDecodeStrategy::BeamSearch { beam_size } => {
            whisper_rs::SamplingStrategy::BeamSearch {
                beam_size,
                // whisper.cpp's default patience (unbounded beam pruning off).
                patience: -1.0,
            }
        }
    });

    params.set_n_threads(settings.n_threads);
    params.set_language(Some("en"));
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, samples)
        .map_err(|error| AsrError::Runtime(error.to_string()))?;

    Ok(state
        .as_iter()
        .map(|segment| RawSegment {
            text: segment.to_string(),
            start_cs: segment.start_timestamp(),
            end_cs: segment.end_timestamp(),
        })
        .collect())
}

/// Map raw Whisper segments (text plus centisecond timestamps) to a
/// [`FinalTranscription`]. whisper.cpp reports t0/t1 in 10 ms ticks, so they
/// are converted to milliseconds here; negative ticks (seen on some models
/// before audio start) clamp to zero.
#[cfg(any(test, feature = "local-whisper-runtime"))]
pub(super) fn transcript_from_whisper_segments(
    segments: impl IntoIterator<Item = RawSegment>,
) -> FinalTranscription {
    FinalTranscription::from_segments(
        segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                text: segment.text,
                start_ms: segment.start_cs.clamp(0, i64::MAX) as u64 * 10,
                end_ms: segment.end_cs.clamp(0, i64::MAX) as u64 * 10,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(text: &str, start_cs: i64, end_cs: i64) -> RawSegment {
        RawSegment {
            text: text.to_string(),
            start_cs,
            end_cs,
        }
    }

    #[test]
    fn whisper_segments_preserve_ordered_text_and_timing() {
        let transcription = transcript_from_whisper_segments(vec![
            segment(" Hello", 0, 150),
            segment(" from slugtale.", 160, 320),
        ]);

        assert_eq!(
            transcription.segments,
            vec![
                TranscriptSegment {
                    text: " Hello".to_string(),
                    start_ms: 0,
                    end_ms: 1_500
                },
                TranscriptSegment {
                    text: " from slugtale.".to_string(),
                    start_ms: 1_600,
                    end_ms: 3_200
                },
            ]
        );
    }

    #[test]
    fn flattening_whisper_segments_matches_the_previous_immediate_join() {
        // Before segments were preserved the runtime joined segment texts with
        // no separator and trimmed once; the flattened text must stay identical
        // so behavior is unchanged until cleanup consumes boundaries.
        let transcription = transcript_from_whisper_segments(vec![
            segment(" Hello ", 0, 100),
            segment(" from slugtale. ", 110, 250),
        ]);

        assert_eq!(transcription.text, "Hello  from slugtale.");
    }

    #[test]
    fn negative_whisper_timestamps_clamp_to_zero() {
        let transcription = transcript_from_whisper_segments(vec![segment(" Hi.", -5, -1)]);

        assert_eq!(transcription.segments[0].start_ms, 0);
        assert_eq!(transcription.segments[0].end_ms, 0);
    }
}
