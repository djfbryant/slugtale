//! What this machine can give the local Whisper decoder: the decode strategy the
//! Transcription Speed Profile asks for, and the thread count to run it with.
//!
//! Every answer here is a pure function of the machine and the profile. The
//! only thing that reads the machine is
//! [`recommended_whisper_decode_settings`], and it is asked per transcription
//! rather than caching a count, so a process that gains or loses CPUs does not
//! keep decoding on yesterday's number.

#[cfg(any(test, feature = "local-whisper-runtime"))]
use crate::SpeedProfile;
#[cfg(any(test, feature = "local-whisper-runtime"))]
use std::num::NonZeroUsize;

#[cfg(any(test, feature = "local-whisper-runtime"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WhisperDecodeStrategy {
    Greedy { best_of: i32 },
    BeamSearch { beam_size: i32 },
}

#[cfg(any(test, feature = "local-whisper-runtime"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WhisperDecodeSettings {
    pub(super) strategy: WhisperDecodeStrategy,
    pub(super) n_threads: i32,
}

/// Map a Transcription Speed Profile to the decode strategy the local Whisper
/// runtime uses: a wider Beam Search is more accurate but slower (CONTEXT.md);
/// Fast skips Beam Search entirely with greedy decoding. Values were picked
/// from measured latency on real speech clips — greedy is fastest, beam 2 costs
/// little over greedy, and beam 5 (the pre-profile default) is 25-45% slower on
/// longer clips (docs/research/whisper-decode-benchmark.md). Note whisper.cpp
/// ignores greedy `best_of` at its default temperature, so meaningfully wider
/// search requires the BeamSearch strategy, not a larger `best_of`.
#[cfg(any(test, feature = "local-whisper-runtime"))]
fn decode_strategy_for_speed_profile(profile: SpeedProfile) -> WhisperDecodeStrategy {
    match profile {
        SpeedProfile::Fast => WhisperDecodeStrategy::Greedy { best_of: 1 },
        SpeedProfile::Balanced => WhisperDecodeStrategy::BeamSearch { beam_size: 2 },
        SpeedProfile::Accurate => WhisperDecodeStrategy::BeamSearch { beam_size: 5 },
    }
}

#[cfg(feature = "local-whisper-runtime")]
pub(super) fn recommended_whisper_decode_settings(profile: SpeedProfile) -> WhisperDecodeSettings {
    whisper_decode_settings_for_available_threads(
        profile,
        whisper_thread_count(
            num_cpus::get_physical(),
            std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
        ),
    )
}

/// How many threads Whisper decoding should use: the physical core count,
/// clamped to the parallelism this process may actually use. ggml's compute
/// threads contend on shared execution units, so running one per SMT sibling
/// is much slower than one per core — 4x slower on the 6C/12T Linux reference
/// machine (slugtale-jwy). `physical_cores` of 0 means detection failed; fall
/// back to the available parallelism.
#[cfg(any(test, feature = "local-whisper-runtime"))]
fn whisper_thread_count(physical_cores: usize, available: NonZeroUsize) -> NonZeroUsize {
    NonZeroUsize::new(physical_cores.min(available.get())).unwrap_or(available)
}

#[cfg(any(test, feature = "local-whisper-runtime"))]
fn whisper_decode_settings_for_available_threads(
    profile: SpeedProfile,
    available_threads: NonZeroUsize,
) -> WhisperDecodeSettings {
    WhisperDecodeSettings {
        strategy: decode_strategy_for_speed_profile(profile),
        n_threads: available_threads.get().min(i32::MAX as usize) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_profiles_map_to_progressively_wider_decode_search() {
        // Mapping chosen from measured latency on real speech clips
        // (docs/research/whisper-decode-benchmark.md): Fast skips Beam Search
        // entirely, Balanced uses a narrow beam, Accurate uses the widest beam.
        assert_eq!(
            decode_strategy_for_speed_profile(SpeedProfile::Fast),
            WhisperDecodeStrategy::Greedy { best_of: 1 }
        );
        assert_eq!(
            decode_strategy_for_speed_profile(SpeedProfile::Balanced),
            WhisperDecodeStrategy::BeamSearch { beam_size: 2 }
        );
        assert_eq!(
            decode_strategy_for_speed_profile(SpeedProfile::Accurate),
            WhisperDecodeStrategy::BeamSearch { beam_size: 5 }
        );
    }

    #[test]
    fn whisper_threads_prefer_physical_cores_over_smt_siblings() {
        // ggml's compute threads contend on shared FP units, so hyperthread
        // siblings slow decoding down instead of speeding it up: on the 6C/12T
        // Linux reference box an 11s clip took 6.1s with 12 threads vs 1.6s with
        // 6 (slugtale-jwy). Use the physical core count, never the SMT total.
        assert_eq!(
            whisper_thread_count(6, NonZeroUsize::new(12).unwrap()),
            NonZeroUsize::new(6).unwrap()
        );
    }

    #[test]
    fn whisper_threads_never_exceed_available_parallelism() {
        // A containerized/affinity-restricted process can see fewer logical CPUs
        // than the machine has physical cores; stay within what we may use.
        assert_eq!(
            whisper_thread_count(8, NonZeroUsize::new(4).unwrap()),
            NonZeroUsize::new(4).unwrap()
        );
    }

    #[test]
    fn whisper_threads_fall_back_to_available_parallelism_when_physical_unknown() {
        assert_eq!(
            whisper_thread_count(0, NonZeroUsize::new(8).unwrap()),
            NonZeroUsize::new(8).unwrap()
        );
    }

    #[test]
    fn decode_settings_use_selected_profile_and_available_threads() {
        let threads = NonZeroUsize::new(4).unwrap();
        let settings =
            whisper_decode_settings_for_available_threads(SpeedProfile::Accurate, threads);

        assert_eq!(
            settings.strategy,
            WhisperDecodeStrategy::BeamSearch { beam_size: 5 }
        );
        assert_eq!(settings.n_threads, 4);
    }

    #[test]
    fn fast_profile_decode_settings_prioritize_low_latency_dictation() {
        let settings = whisper_decode_settings_for_available_threads(
            SpeedProfile::Fast,
            NonZeroUsize::new(10).unwrap(),
        );

        assert_eq!(
            settings,
            WhisperDecodeSettings {
                strategy: WhisperDecodeStrategy::Greedy { best_of: 1 },
                n_threads: 10,
            }
        );
    }
}
