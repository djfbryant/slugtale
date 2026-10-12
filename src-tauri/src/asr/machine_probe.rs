
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
        assert_eq!(
            whisper_thread_count(6, NonZeroUsize::new(12).unwrap()),
            NonZeroUsize::new(6).unwrap()
        );
    }

    #[test]
    fn whisper_threads_never_exceed_available_parallelism() {
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
