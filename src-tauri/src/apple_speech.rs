//! **How it reaches Apple's API.** `SpeechAnalyzer` and `SpeechTranscriber` are
//! Swift-only — they are not exported to Objective-C, so the `objc2-speech`
//! crate cannot see them (it binds the older `SFSpeechRecognizer`, a different
//! engine that *does* have a server path). The supported route is a small Swift
//! library with a C ABI, which lives in `swift/SlugtaleAppleSpeech.swift` and is
//! compiled into a static archive by `build.rs`. This module declares those four
//! functions and owns every policy decision above them.
//! **Assets are per-application, not per-machine.** Measured on macOS 26.5:
//! `AssetInventory` reports a locale as not installed to an application that has
//! never asked for it, *even when another application on the same Mac already
//! downloaded it*. The first answer Settings shows on a fresh install is
//! therefore [`EngineUnavailable::AssetsMissing`] rather than
//! [`EngineAvailability::Available`], and the install action clears it — in
//! roughly a third of a second when macOS already holds the files, and in a real
//! download when it does not. A developer-run build (ADR-0020) is identified by
//! its ad-hoc signature, so this reappears after a rebuild; a signed Slugtale
//! keeps its registration across updates.

use crate::{
    AsrError, AssetInstall, CapturedAudio, DownloadProgress, EngineAssetLifecycle, EngineAssets,
    EngineAvailability, EngineMetadata, EngineTranscriber, EngineTranscription, TranscriptionEngine,
};
#[cfg_attr(
    all(target_os = "macos", feature = "apple-speech-runtime"),
    allow(unused_imports)
)]
use crate::EngineUnavailable;
use std::sync::Mutex;

macro_rules! bridge_convention {
    ($($item:item)*) => {
        $(
            #[cfg_attr(
                not(all(target_os = "macos", feature = "apple-speech-runtime")),
                allow(dead_code)
            )]
            $item
        )*
    };
}

const APPLE_SPEECH_ENGINE: TranscriptionEngine = TranscriptionEngine::AppleSpeech;

const APPLE_SPEECH_SUPPORTED_PLATFORMS: &str = "macOS 26 and later";

const DEFAULT_APPLE_SPEECH_LOCALE: &str = "en-US";

bridge_convention! {
    const APPLE_SPEECH_REQUIRED_OS: &str = "macOS 26 or later";

    const ALTERNATIVE_SEPARATOR: char = '\u{1E}';

    const CONFIDENCE_UNREPORTED: f64 = -1.0;

    const STATUS_OK: i32 = 0;
    const STATUS_UNSUPPORTED_OS: i32 = 1;
    const STATUS_UNSUPPORTED_LOCALE: i32 = 2;
    const STATUS_ASSETS_MISSING: i32 = 3;
    const STATUS_FAILED: i32 = 4;
    const STATUS_UNSUPPORTED_HARDWARE: i32 = 5;
}

#[cfg(any(test, not(all(target_os = "macos", feature = "apple-speech-runtime"))))]
fn unsupported_platform_reason() -> EngineUnavailable {
    match EngineAvailability::unsupported_platform(
        APPLE_SPEECH_ENGINE,
        APPLE_SPEECH_SUPPORTED_PLATFORMS,
    ) {
        EngineAvailability::Unavailable(reason) => reason,
        EngineAvailability::Available => {
            unreachable!("unsupported_platform never reports an engine as available")
        }
    }
}

pub struct AppleSpeechProvider {
    locale: String,
    cached_availability: Mutex<Option<EngineAvailability>>,
}

impl Default for AppleSpeechProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AppleSpeechProvider {
    pub fn new() -> Self {
        Self::for_locale(DEFAULT_APPLE_SPEECH_LOCALE)
    }

    pub fn for_locale(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
            cached_availability: Mutex::new(None),
        }
    }

    pub fn locale(&self) -> &str {
        &self.locale
    }

    fn refresh_availability(&self) -> EngineAvailability {
        let probed = probe_availability(&self.locale);
        *self.lock_cache() = Some(probed.clone());
        probed
    }

    /// Ask macOS to download and install the system speech assets for this
    /// provider's Dictation Language, returning the availability that results.
    ///
    /// **This must only ever be called from an explicit user action.** It spends
    /// the user's bandwidth and disk on assets that can run to hundreds of
    /// megabytes, so Slugtale offers it as a button in Settings — the one
    /// recoverable [`EngineUnavailable::AssetsMissing`] case — and never fires
    /// it speculatively or on a failed dictation.
    ///
    /// The assets remain owned and updated by macOS; Slugtale learns only
    /// whether they are present.
    fn request_asset_installation(&self) -> Result<EngineAvailability, String> {
        install_assets(&self.locale)?;
        Ok(self.refresh_availability())
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Option<EngineAvailability>> {
        self.cached_availability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl EngineTranscriber for AppleSpeechProvider {
    fn engine(&self) -> TranscriptionEngine {
        APPLE_SPEECH_ENGINE
    }

    fn metadata(&self) -> EngineMetadata {
        EngineMetadata {
            engine: APPLE_SPEECH_ENGINE,
            capability: "The speech recognition built into macOS, installed, updated, \
                         and managed by the system itself. Nothing to download, no extra \
                         disk space used beyond what macOS already provides.",
            model_id: "com.apple.speech.SpeechTranscriber",
            revision: "system-managed by macOS",
            approximate_bytes: None,
            source_url: None,
            license: "Apple system service. The speech models are installed, \
                      stored, and updated by macOS under the macOS Software \
                      Licence Agreement; Slugtale neither bundles nor \
                      redistributes them.",
            license_url: "https://www.apple.com/legal/sla/",
            attribution: None,
            modifications: None,
            system_managed: true,
            supported_platforms: APPLE_SPEECH_SUPPORTED_PLATFORMS,
        }
    }

    fn availability(&self) -> EngineAvailability {
        let mut cached = self.lock_cache();
        if let Some(known) = cached.as_ref() {
            return known.clone();
        }
        let probed = probe_availability(&self.locale);
        *cached = Some(probed.clone());
        probed
    }

    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        if let EngineAvailability::Unavailable(reason) = self.availability() {
            return Err(AsrError::EngineUnavailable {
                engine: APPLE_SPEECH_ENGINE,
                reason,
            });
        }
        validate_recording(audio)?;
        transcribe_with_apple_speech(&self.locale, audio)
    }
}

impl EngineAssetLifecycle for AppleSpeechProvider {
    fn assets(&self) -> EngineAssets {
        EngineAssets::unmeasured()
    }

    fn can_install_assets(&self) -> bool {
        true
    }

    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        let _ = on_progress;
        self.request_asset_installation()?;
        Ok(AssetInstall {
            warm_up: false,
        })
    }

    fn remove_assets(&self) -> Result<(), String> {
        Err(
            "Apple SpeechTranscriber's assets are installed and managed by macOS; \
             Slugtale cannot remove them."
                .to_string(),
        )
    }
}

fn validate_recording(audio: &CapturedAudio) -> Result<(), AsrError> {
    if audio.sample_rate_hz == 0 {
        return Err(AsrError::UnsupportedAudio(
            "the recording has no sample rate".to_string(),
        ));
    }
    if audio.samples.is_empty() {
        return Err(AsrError::UnsupportedAudio(
            "the recording contains no audio".to_string(),
        ));
    }
    Ok(())
}

bridge_convention! {
    fn reported_confidence(value: f64) -> Option<f32> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return None;
        }
        Some(value as f32)
    }

    fn split_alternatives(joined: &str) -> Vec<String> {
        joined
            .split(ALTERNATIVE_SEPARATOR)
            .map(str::trim)
            .filter(|alternative| !alternative.is_empty())
            .map(str::to_string)
            .collect()
    }
}


#[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
mod bridge {

    use super::{
        reported_confidence, split_alternatives, APPLE_SPEECH_ENGINE, APPLE_SPEECH_REQUIRED_OS,
        CONFIDENCE_UNREPORTED, STATUS_ASSETS_MISSING, STATUS_FAILED, STATUS_OK,
        STATUS_UNSUPPORTED_HARDWARE, STATUS_UNSUPPORTED_LOCALE, STATUS_UNSUPPORTED_OS,
    };
    use crate::{
        AsrError, CapturedAudio, EngineAvailability, EngineConfidence, EngineTranscription,
        EngineUnavailable, FinalTranscription,
    };
    use std::ffi::{c_char, CStr, CString};
    use std::ptr;

    extern "C" {
        fn slugtale_apple_speech_probe(
            locale_identifier: *const c_char,
            out_detail: *mut *mut c_char,
        ) -> i32;

        #[allow(clippy::too_many_arguments)]
        fn slugtale_apple_speech_transcribe(
            samples: *const f32,
            sample_count: isize,
            sample_rate_hz: f64,
            locale_identifier: *const c_char,
            out_text: *mut *mut c_char,
            out_alternatives: *mut *mut c_char,
            out_mean_confidence: *mut f64,
            out_minimum_confidence: *mut f64,
            out_detail: *mut *mut c_char,
        ) -> i32;

        fn slugtale_apple_speech_install_assets(
            locale_identifier: *const c_char,
            out_detail: *mut *mut c_char,
        ) -> i32;

        fn slugtale_apple_speech_free(pointer: *mut c_char);
    }

    /// Take ownership of a string the bridge allocated, copy it into Rust, and
    /// hand the original back to be freed. Every owned pointer the bridge
    /// returns goes through here, which is why there is exactly one `free` per
    /// `strdup` and no path that leaks one.
    fn take_string(pointer: *mut c_char) -> Option<String> {
        if pointer.is_null() {
            return None;
        }
        // Safe: the bridge only ever writes NUL-terminated `strdup` results
        // here, and this is the single place that consumes one.
        let owned = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        unsafe { slugtale_apple_speech_free(pointer) };
        Some(owned)
    }

    struct DrainedOutParameters {
        text: Option<String>,
        alternatives: Option<String>,
        detail: Option<String>,
    }

    fn drain_out_parameters(
        text: *mut c_char,
        alternatives: *mut c_char,
        detail: *mut c_char,
    ) -> DrainedOutParameters {
        DrainedOutParameters {
            text: take_string(text),
            alternatives: take_string(alternatives),
            detail: take_string(detail),
        }
    }

    fn locale_argument(locale: &str) -> Result<CString, EngineUnavailable> {
        CString::new(locale).map_err(|_| EngineUnavailable::UnsupportedLocale {
            detected: locale.to_string(),
        })
    }

    pub(super) fn probe_availability(locale: &str) -> EngineAvailability {
        let identifier = match locale_argument(locale) {
            Ok(identifier) => identifier,
            Err(reason) => return EngineAvailability::Unavailable(reason),
        };

        let mut detail: *mut c_char = ptr::null_mut();
        // Safe: `identifier` outlives the call and `detail` is a valid slot the
        // bridge either fills with an owned string or leaves null.
        let status =
            unsafe { slugtale_apple_speech_probe(identifier.as_ptr(), ptr::addr_of_mut!(detail)) };
        let detail = take_string(detail);

        match status {
            STATUS_OK => EngineAvailability::Available,
            _ => EngineAvailability::Unavailable(unavailable_reason(status, detail, locale)),
        }
    }

    fn unavailable_reason(status: i32, detail: Option<String>, locale: &str) -> EngineUnavailable {
        match status {
            STATUS_UNSUPPORTED_OS => EngineUnavailable::UnsupportedOsVersion {
                required: APPLE_SPEECH_REQUIRED_OS.to_string(),
                detected: detail.unwrap_or_else(|| "an older macOS".to_string()),
            },
            STATUS_UNSUPPORTED_LOCALE => EngineUnavailable::UnsupportedLocale {
                detected: detail.unwrap_or_else(|| locale.to_string()),
            },
            STATUS_ASSETS_MISSING => EngineUnavailable::AssetsMissing {
                detail: detail.unwrap_or_else(|| {
                    "macOS has not installed the speech assets for this language yet.".to_string()
                }),
            },
            STATUS_UNSUPPORTED_HARDWARE => EngineUnavailable::UnsupportedPlatform {
                detail: detail.unwrap_or_else(|| {
                    "This Mac's hardware cannot run the on-device transcriber.".to_string()
                }),
            },
            STATUS_FAILED => EngineUnavailable::ProbeFailed {
                detail: detail.unwrap_or_else(|| {
                    "macOS could not report whether the speech assets are usable.".to_string()
                }),
            },
            other => EngineUnavailable::ProbeFailed {
                detail: format!("the Apple speech bridge returned an unrecognised status {other}"),
            },
        }
    }

    pub(super) fn install_assets(locale: &str) -> Result<(), String> {
        let identifier = locale_argument(locale).map_err(|reason| reason.to_string())?;

        let mut detail: *mut c_char = ptr::null_mut();
        // Safe: same contract as the probe. This blocks for as long as the
        // download takes, which is why callers run it off the dictation path.
        let status = unsafe {
            slugtale_apple_speech_install_assets(identifier.as_ptr(), ptr::addr_of_mut!(detail))
        };
        let detail = take_string(detail);

        if status == STATUS_OK {
            Ok(())
        } else {
            Err(unavailable_reason(status, detail, locale).to_string())
        }
    }

    pub(super) fn transcribe(
        locale: &str,
        audio: &CapturedAudio,
    ) -> Result<EngineTranscription, AsrError> {
        let identifier = locale_argument(locale).map_err(|reason| AsrError::EngineUnavailable {
            engine: APPLE_SPEECH_ENGINE,
            reason,
        })?;

        let mut text: *mut c_char = ptr::null_mut();
        let mut alternatives: *mut c_char = ptr::null_mut();
        let mut detail: *mut c_char = ptr::null_mut();
        let mut mean = CONFIDENCE_UNREPORTED;
        let mut minimum = CONFIDENCE_UNREPORTED;

        let started = std::time::Instant::now();
        // Safe: the sample slice is borrowed for the duration of the call and
        // the bridge copies it before doing any async work; every out-parameter
        // is a live slot, and every owned pointer written into one is consumed
        // exactly once by `take_string` below.
        let status = unsafe {
            slugtale_apple_speech_transcribe(
                audio.samples.as_ptr(),
                audio.samples.len() as isize,
                f64::from(audio.sample_rate_hz),
                identifier.as_ptr(),
                ptr::addr_of_mut!(text),
                ptr::addr_of_mut!(alternatives),
                ptr::addr_of_mut!(mean),
                ptr::addr_of_mut!(minimum),
                ptr::addr_of_mut!(detail),
            )
        };
        let latency = started.elapsed();

        let DrainedOutParameters {
            text,
            alternatives,
            detail,
        } = drain_out_parameters(text, alternatives, detail);

        match status {
            STATUS_OK => {}
            STATUS_FAILED => {
                return Err(AsrError::Runtime(detail.unwrap_or_else(|| {
                    "Apple SpeechTranscriber could not transcribe the recording.".to_string()
                })))
            }
            other => {
                return Err(AsrError::EngineUnavailable {
                    engine: APPLE_SPEECH_ENGINE,
                    reason: unavailable_reason(other, detail, locale),
                })
            }
        }

        let text = text.ok_or_else(|| {
            AsrError::Runtime("Apple SpeechTranscriber returned no transcription.".to_string())
        })?;

        Ok(EngineTranscription {
            engine: APPLE_SPEECH_ENGINE,
            transcription: FinalTranscription::plain(text.trim()),
            alternatives: alternatives
                .as_deref()
                .map(split_alternatives)
                .unwrap_or_default(),
            confidence: EngineConfidence {
                mean: reported_confidence(mean),
                minimum: reported_confidence(minimum),
            },
            latency,
        })
    }
}

#[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
fn probe_availability(locale: &str) -> EngineAvailability {
    bridge::probe_availability(locale)
}

#[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
fn install_assets(locale: &str) -> Result<(), String> {
    bridge::install_assets(locale)
}

#[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
fn transcribe_with_apple_speech(
    locale: &str,
    audio: &CapturedAudio,
) -> Result<EngineTranscription, AsrError> {
    bridge::transcribe(locale, audio)
}


#[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
fn unreachable_engine_reason() -> EngineUnavailable {
    if cfg!(target_os = "macos") {
        EngineUnavailable::RuntimeNotBuilt
    } else {
        unsupported_platform_reason()
    }
}

#[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
fn probe_availability(_locale: &str) -> EngineAvailability {
    EngineAvailability::Unavailable(unreachable_engine_reason())
}

#[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
fn install_assets(_locale: &str) -> Result<(), String> {
    Err(unreachable_engine_reason().to_string())
}

#[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
fn transcribe_with_apple_speech(
    _locale: &str,
    _audio: &CapturedAudio,
) -> Result<EngineTranscription, AsrError> {
    Err(AsrError::EngineUnavailable {
        engine: APPLE_SPEECH_ENGINE,
        reason: unreachable_engine_reason(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neither_half_of_the_engine_ever_prints_anything() {
        let rust = include_str!("apple_speech.rs");
        for stem in ["print", "eprint", "dbg"] {
            for macro_name in [format!("{stem}!"), format!("{stem}ln!")] {
                assert!(
                    !rust.contains(&macro_name),
                    "{macro_name} must not appear in the Apple SpeechTranscriber engine"
                );
            }
        }

        let swift = include_str!("../swift/SlugtaleAppleSpeech.swift");
        for stem in ["print", "debugPrint", "dump", "NSLog", "os_log", "Logger"] {
            let call = format!("{stem}(");
            assert!(
                !swift.contains(&call),
                "{call} must not appear in the Swift bridge"
            );
        }
    }

    #[test]
    fn on_linux_and_windows_the_engine_names_itself_and_where_it_runs() {
        assert_eq!(
            EngineAvailability::Unavailable(unsupported_platform_reason()),
            EngineAvailability::Unavailable(EngineUnavailable::UnsupportedPlatform {
                detail: "Apple SpeechTranscriber is available only on macOS 26 and later"
                    .to_string(),
            })
        );
        assert!(!EngineAvailability::Unavailable(unsupported_platform_reason()).is_available());
    }

    #[test]
    fn an_unsupported_platform_offers_the_user_nothing_to_fix() {
        assert!(!unsupported_platform_reason().is_user_resolvable());
    }

    #[test]
    fn metadata_says_the_assets_belong_to_macos() {
        let metadata = AppleSpeechProvider::new().metadata();

        assert_eq!(metadata.engine, TranscriptionEngine::AppleSpeech);
        assert!(metadata.system_managed);
        assert_eq!(metadata.source_url, None, "Slugtale never downloads these");
        assert_eq!(metadata.approximate_bytes, None);
        assert_eq!(metadata.attribution, None, "Apple asks for no credit");
        assert_eq!(
            metadata.modifications, None,
            "the engine is used as shipped"
        );
        assert_eq!(
            metadata.supported_platforms,
            APPLE_SPEECH_SUPPORTED_PLATFORMS
        );
        assert!(metadata.license.contains("macOS"));
    }

    #[test]
    fn removing_assets_it_does_not_own_is_refused_in_apples_own_words() {
        let error = AppleSpeechProvider::new().remove_assets().unwrap_err();

        assert_eq!(
            error,
            "Apple SpeechTranscriber's assets are installed and managed by macOS; \
             Slugtale cannot remove them."
        );
    }

    #[test]
    fn the_assets_are_macos_to_measure_and_macos_to_install() {
        let provider = AppleSpeechProvider::new();

        assert_eq!(provider.assets(), EngineAssets::unmeasured());
        assert!(provider.can_install_assets());
        assert!(provider.metadata().system_managed);
    }

    #[test]
    fn the_default_dictation_language_is_a_full_locale_identifier() {
        let provider = AppleSpeechProvider::new();
        assert_eq!(provider.locale(), "en-US");
        assert!(provider.locale().contains('-'));

        assert_eq!(AppleSpeechProvider::for_locale("fr-FR").locale(), "fr-FR");
    }

    #[test]
    fn an_engine_that_reports_no_confidence_is_not_an_uncertain_engine() {
        assert_eq!(reported_confidence(CONFIDENCE_UNREPORTED), None);
        assert_eq!(reported_confidence(f64::NAN), None);
        assert_eq!(reported_confidence(1.5), None);

        assert_eq!(reported_confidence(0.0), Some(0.0));
        assert_eq!(reported_confidence(1.0), Some(1.0));
        assert_eq!(reported_confidence(0.864), Some(0.864));
    }

    #[test]
    fn the_unreported_sentinel_can_never_be_mistaken_for_a_score() {
        assert!(!(0.0..=1.0).contains(&CONFIDENCE_UNREPORTED));
        assert!(CONFIDENCE_UNREPORTED.is_finite());
    }

    #[test]
    fn the_alternative_separator_is_a_control_character_a_transcript_never_holds() {
        assert!(ALTERNATIVE_SEPARATOR.is_control());
        assert!(!ALTERNATIVE_SEPARATOR.is_whitespace());
        assert_eq!(ALTERNATIVE_SEPARATOR, '\u{1E}');
    }

    #[test]
    fn the_status_codes_match_the_numbers_the_swift_bridge_returns() {
        let swift = include_str!("../swift/SlugtaleAppleSpeech.swift");
        for (name, rust_value) in [
            ("STATUS_OK", STATUS_OK),
            ("STATUS_UNSUPPORTED_OS", STATUS_UNSUPPORTED_OS),
            ("STATUS_UNSUPPORTED_LOCALE", STATUS_UNSUPPORTED_LOCALE),
            ("STATUS_ASSETS_MISSING", STATUS_ASSETS_MISSING),
            ("STATUS_FAILED", STATUS_FAILED),
            ("STATUS_UNSUPPORTED_HARDWARE", STATUS_UNSUPPORTED_HARDWARE),
        ] {
            let declaration = format!("private let {name}: Int32 = ");
            let swift_value = swift
                .lines()
                .find_map(|line| line.strip_prefix(&declaration))
                .and_then(|value| value.trim().parse::<i32>().ok());
            assert_eq!(
                swift_value,
                Some(rust_value),
                "{name} differs between apple_speech.rs and SlugtaleAppleSpeech.swift"
            );
        }
    }

    #[test]
    fn alternatives_arrive_as_whole_transcripts_split_on_the_record_separator() {
        assert_eq!(
            split_alternatives(
                "Slug tail turns speech into text\u{1E}SlugTail turns speech into text"
            ),
            vec![
                "Slug tail turns speech into text".to_string(),
                "SlugTail turns speech into text".to_string(),
            ]
        );
        assert!(split_alternatives("").is_empty());
        assert!(split_alternatives("\u{1E}\u{1E}").is_empty());
    }

    #[test]
    fn a_recording_with_nothing_in_it_never_reaches_the_bridge() {
        assert_eq!(
            validate_recording(&CapturedAudio::mono_16khz(Vec::new())),
            Err(AsrError::UnsupportedAudio(
                "the recording contains no audio".to_string()
            ))
        );
        assert_eq!(
            validate_recording(&CapturedAudio {
                sample_rate_hz: 0,
                samples: vec![0.0; 16_000],
            }),
            Err(AsrError::UnsupportedAudio(
                "the recording has no sample rate".to_string()
            ))
        );
        assert_eq!(
            validate_recording(&CapturedAudio {
                sample_rate_hz: 44_100,
                samples: vec![0.0; 4_410],
            }),
            Ok(())
        );
    }

    #[test]
    fn availability_is_probed_once_and_then_read_from_cache() {
        let provider = AppleSpeechProvider::new();
        let first = provider.availability();
        assert_eq!(provider.availability(), first);
        assert_eq!(provider.refresh_availability(), first);
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
    #[test]
    fn a_build_that_cannot_reach_apple_speech_refuses_before_touching_the_audio() {
        let provider = AppleSpeechProvider::new();
        let error = provider
            .transcribe(&CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .unwrap_err();

        let AsrError::EngineUnavailable { engine, reason } = error else {
            panic!("a build without the Apple runtime must report the engine unavailable");
        };
        assert_eq!(engine, TranscriptionEngine::AppleSpeech);

        #[cfg(target_os = "macos")]
        assert_eq!(reason, EngineUnavailable::RuntimeNotBuilt);
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            reason,
            EngineUnavailable::UnsupportedPlatform {
                detail: "Apple SpeechTranscriber is available only on macOS 26 and later"
                    .to_string(),
            }
        );
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
    #[test]
    fn a_build_that_cannot_reach_apple_speech_cannot_install_its_assets_either() {
        let installed = AppleSpeechProvider::new().request_asset_installation();
        assert!(installed.is_err(), "there is nothing here to install into");
    }

    #[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
    #[test]
    #[ignore = "needs macOS 26 with the en-US speech assets installed"]
    fn a_real_recording_comes_back_as_text_with_confidence() {
        let provider = AppleSpeechProvider::new();
        let availability = match provider.availability() {
            EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing { .. }) => provider
                .request_asset_installation()
                .expect("macOS could install the en-US speech assets"),
            other => other,
        };
        assert!(
            availability.is_available(),
            "this machine cannot run the engine: {availability:?}"
        );

        let audio = synthesized_speech("Slugtale turns spoken words into text");
        let result = provider.transcribe(&audio).expect("a real transcription");

        assert_eq!(result.engine, TranscriptionEngine::AppleSpeech);
        assert!(
            result
                .text()
                .to_lowercase()
                .contains("spoken words into text"),
            "the engine heard something unexpected"
        );
        let score = result
            .confidence
            .escalation_score()
            .expect("SpeechTranscriber reports confidence when asked for it");
        assert!((0.0..=1.0).contains(&score), "confidence out of range");
        assert!(result.latency > std::time::Duration::ZERO);
    }

    #[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
    fn synthesized_speech(sentence: &str) -> CapturedAudio {
        let directory = std::env::temp_dir().join(format!(
            "slugtale-apple-speech-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).expect("a temporary directory");
        let spoken = directory.join("spoken.aiff");
        let wave = directory.join("spoken.wav");

        assert!(std::process::Command::new("say")
            .arg("-o")
            .arg(&spoken)
            .arg(sentence)
            .status()
            .expect("say is a macOS component")
            .success());
        assert!(std::process::Command::new("afconvert")
            .args(["-f", "WAVE", "-d", "LEI16@16000", "-c", "1"])
            .arg(&spoken)
            .arg(&wave)
            .status()
            .expect("afconvert is a macOS component")
            .success());

        let bytes = std::fs::read(&wave).expect("the converted recording");
        std::fs::remove_dir_all(&directory).ok();
        CapturedAudio::mono_16khz(mono_16_bit_wave_samples(&bytes))
    }

    #[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
    fn mono_16_bit_wave_samples(bytes: &[u8]) -> Vec<f32> {
        let mut cursor = 12;
        while cursor + 8 <= bytes.len() {
            let id = &bytes[cursor..cursor + 4];
            let size =
                u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
            let body = cursor + 8;
            if id == b"data" {
                let end = (body + size).min(bytes.len());
                return bytes[body..end]
                    .chunks_exact(2)
                    .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0)
                    .collect();
            }
            cursor = body + size + (size & 1);
        }
        panic!("the converted recording had no data chunk");
    }

    #[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
    #[test]
    fn a_real_probe_answers_in_the_boundary_s_vocabulary() {
        let availability = AppleSpeechProvider::new().availability();

        match availability {
            EngineAvailability::Available => {}
            EngineAvailability::Unavailable(reason) => {
                assert!(
                    !reason.to_string().is_empty(),
                    "an unavailable engine must explain itself to Settings"
                );
                assert!(
                    !matches!(reason, EngineUnavailable::RuntimeNotBuilt),
                    "this build has the runtime; it must not claim otherwise"
                );
            }
        }
    }
}
