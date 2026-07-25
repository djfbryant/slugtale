//! Apple SpeechTranscriber as a Transcription Engine (slugtale-vjs.2).
//!
//! Apple ships a speech recognizer with macOS 26 that runs entirely on the
//! user's machine and whose model assets the operating system installs, stores,
//! and updates. That makes it the one engine Slugtale can offer without asking
//! the user to download anything from Slugtale, and the one engine whose weights
//! Slugtale must never touch: the assets stay Apple's, and nothing here reads,
//! copies, bundles, or redistributes them.
//!
//! **The provider type exists on every platform.** Slugtale ships on Linux and
//! Windows too, and Settings has to be able to say *why* an engine is missing
//! rather than silently omitting it. So [`AppleSpeechProvider`] compiles
//! everywhere; what varies is the answer it gives:
//!
//! | Build | [`TranscriptionProvider::availability`] |
//! |---|---|
//! | Linux, Windows | [`EngineUnavailable::UnsupportedPlatform`] |
//! | macOS without the `apple-speech-runtime` feature | [`EngineUnavailable::RuntimeNotBuilt`] |
//! | macOS with the feature | whatever the machine actually reports |
//!
//! Every call into Apple's API is behind **both** `cfg(target_os = "macos")` and
//! `cfg(feature = "apple-speech-runtime")`, so turning the feature on in a Linux
//! build is a no-op rather than a build failure.
//!
//! **How it reaches Apple's API.** `SpeechAnalyzer` and `SpeechTranscriber` are
//! Swift-only — they are not exported to Objective-C, so the `objc2-speech`
//! crate cannot see them (it binds the older `SFSpeechRecognizer`, a different
//! engine that *does* have a server path). The supported route is a small Swift
//! library with a C ABI, which lives in `swift/SlugtaleAppleSpeech.swift` and is
//! compiled into a static archive by `build.rs`. This module declares those five
//! functions and owns every policy decision above them.
//!
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
//!
//! **Nothing leaves the machine.** `SpeechAnalyzer` has no server mode to
//! misconfigure, and on top of that the bridge refuses to transcribe unless
//! macOS reports the locale's assets as *installed* — so a machine that is still
//! fetching them is reported unavailable rather than allowed to fall back to
//! anything. Slugtale never uses this engine's output to train, fine-tune, or
//! improve another model, and no transcript, alternative, or confidence score
//! reaches the Local Diagnostic Log or the network.

use crate::{
    AsrError, CapturedAudio, EngineAvailability, EngineMetadata, EngineTranscription,
    TranscriptionEngine, TranscriptionProvider,
};
// Named separately because only the portable half and the tests refer to it by
// this path; on macOS with the runtime the bridge imports its own. The import
// stays unconditional so the doc links above it keep resolving.
#[cfg_attr(
    all(target_os = "macos", feature = "apple-speech-runtime"),
    allow(unused_imports)
)]
use crate::EngineUnavailable;
use std::sync::Mutex;

/// Items that describe the C ABI's own conventions. They are compiled on every
/// platform so their behaviour stays unit-testable from a Linux or Windows
/// checkout, but only the macOS bridge ever calls them — hence the allowance
/// rather than a `cfg` that would take the tests away with them.
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

/// The engine this module provides.
pub const APPLE_SPEECH_ENGINE: TranscriptionEngine = TranscriptionEngine::AppleSpeech;

/// The operating systems this engine can ever run on. Used in both the Settings
/// metadata and the unsupported-platform reason, so the two cannot drift.
pub const APPLE_SPEECH_SUPPORTED_PLATFORMS: &str = "macOS 26 and later";

/// The Dictation Language the provider assumes when the caller does not name
/// one. Slugtale is English-only by default (ADR-0011), and Apple keys its
/// speech assets by locale rather than by language, so this has to be a full
/// locale identifier rather than a bare `en`.
pub const DEFAULT_APPLE_SPEECH_LOCALE: &str = "en-US";

bridge_convention! {
    /// The macOS this engine needs, phrased for the Settings copy that renders
    /// [`EngineUnavailable::UnsupportedOsVersion`].
    const APPLE_SPEECH_REQUIRED_OS: &str = "macOS 26 or later";

    /// Separates whole-transcript alternatives in the single string the Swift
    /// bridge returns. ASCII RS (0x1E) is a control character no transcriber
    /// emits, which keeps the C ABI to one owned pointer rather than an array of
    /// arrays.
    const ALTERNATIVE_SEPARATOR: char = '\u{1E}';

    /// What the bridge writes to a confidence out-parameter when
    /// `SpeechTranscriber` reported no confidence at all. It sits outside the
    /// valid `0.0..=1.0` range on purpose: a silent engine is not a
    /// low-confidence engine, and the Second Opinion router must not confuse the
    /// two.
    const CONFIDENCE_UNREPORTED: f64 = -1.0;
}

/// The answer [`AppleSpeechProvider::availability`] gives on every operating
/// system that is not macOS.
///
/// Exposed as a function rather than being inlined into a `cfg` branch so the
/// Linux and Windows wording is testable from a macOS developer machine — the
/// only place most of this code is ever compiled.
pub fn apple_speech_unsupported_platform() -> EngineAvailability {
    EngineAvailability::Unavailable(unsupported_platform_reason())
}

/// The same answer as an [`EngineUnavailable`], for the places that need the
/// reason without its wrapper. Built through the boundary's own helper so this
/// provider words the Linux and Windows case exactly as every other one does.
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

/// Apple SpeechTranscriber behind the Transcription Engine boundary.
///
/// Construction is free and touches nothing: the first
/// [`TranscriptionProvider::availability`] call does the real probe — OS
/// version, hardware, locale, installed assets — and every call after it reads
/// the cached answer. That matters because availability is consulted on the
/// dictation fast path as well as in Settings, and asking macOS about its asset
/// inventory takes long enough to be felt if it happened on every dictation.
/// [`AppleSpeechProvider::refresh_availability`] is the way back out of the
/// cache after the user installs the assets.
pub struct AppleSpeechProvider {
    locale: String,
    /// `None` until the first probe. The mutex is held across the probe so a
    /// burst of concurrent first calls costs one system query, not several.
    cached_availability: Mutex<Option<EngineAvailability>>,
}

impl Default for AppleSpeechProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AppleSpeechProvider {
    /// A provider for the default Dictation Language.
    pub fn new() -> Self {
        Self::for_locale(DEFAULT_APPLE_SPEECH_LOCALE)
    }

    /// A provider for a specific Dictation Language, as a locale identifier such
    /// as `en-US`. Apple resolves this loosely — asking for a regional variant
    /// it does not carry can legitimately land on a broader asset — so callers
    /// should pass what the user chose and let macOS decide the match.
    pub fn for_locale(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
            cached_availability: Mutex::new(None),
        }
    }

    /// The Dictation Language this provider asks Apple for.
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// Re-probe the machine and replace the cached availability.
    ///
    /// Availability is otherwise cached forever, which is right for the OS
    /// version and the hardware but wrong for installed assets — those change
    /// the moment [`AppleSpeechProvider::request_asset_installation`] finishes,
    /// or when the user adds a language in System Settings.
    pub fn refresh_availability(&self) -> EngineAvailability {
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
    pub fn request_asset_installation(&self) -> Result<EngineAvailability, String> {
        install_assets(&self.locale)?;
        Ok(self.refresh_availability())
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, Option<EngineAvailability>> {
        // A poisoned cache holds a perfectly good availability answer, and
        // panicking here would take down a dictation over a stale probe.
        self.cached_availability
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl TranscriptionProvider for AppleSpeechProvider {
    fn engine(&self) -> TranscriptionEngine {
        APPLE_SPEECH_ENGINE
    }

    fn metadata(&self) -> EngineMetadata {
        EngineMetadata {
            engine: APPLE_SPEECH_ENGINE,
            model_id: "com.apple.speech.SpeechTranscriber",
            // There is no revision for Slugtale to pin. macOS chooses, installs,
            // and updates these assets; saying "system-managed" is the honest
            // answer, and pretending to a version number would imply a control
            // Slugtale does not have.
            revision: "system-managed by macOS",
            // Apple does not publish an installed size, and it varies by locale
            // and by what other system features already share the assets.
            approximate_bytes: None,
            // Slugtale never downloads these. There is nothing to link to.
            source_url: None,
            license: "Apple system service. The speech models are installed, \
                      stored, and updated by macOS under the macOS Software \
                      Licence Agreement; Slugtale neither bundles nor \
                      redistributes them.",
            license_url: "https://www.apple.com/legal/sla/",
            // Apple asks for no credit for using a system API, so claiming one
            // would be inventing an obligation rather than honouring one.
            attribution: None,
            // Slugtale changes nothing: it calls the system engine as shipped.
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
        // Availability first, and from the cache: an engine that cannot run here
        // must say so without touching the recording, so the Second Opinion
        // router can fall back rather than report a failure to the user.
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

/// Reject a recording the engine cannot do anything with, before it crosses the
/// C ABI. Unlike Whisper, Apple's analyzer converts sample rates itself, so the
/// only thing to police here is that there is audio at all and that its rate is
/// meaningful.
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
    /// Turn one of the bridge's confidence out-parameters into the boundary's
    /// optional score. Anything outside `0.0..=1.0` — including the `-1.0`
    /// sentinel and any NaN — means "not reported", never "reported as bad".
    fn reported_confidence(value: f64) -> Option<f32> {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return None;
        }
        Some(value as f32)
    }

    /// Split the bridge's record-separated alternatives back into whole
    /// transcripts. Empty entries are dropped rather than inserted as blank
    /// alternatives, which would look to the router like the engine proposing
    /// silence.
    fn split_alternatives(joined: &str) -> Vec<String> {
        joined
            .split(ALTERNATIVE_SEPARATOR)
            .map(str::trim)
            .filter(|alternative| !alternative.is_empty())
            .map(str::to_string)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The macOS path: everything below is behind both gates.
// ---------------------------------------------------------------------------

#[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
mod bridge {
    //! The Rust half of the C ABI declared in `swift/SlugtaleAppleSpeech.swift`.
    //! Nothing outside this module touches a raw pointer.

    use super::{
        reported_confidence, split_alternatives, APPLE_SPEECH_ENGINE, APPLE_SPEECH_REQUIRED_OS,
        CONFIDENCE_UNREPORTED,
    };
    use crate::{
        AsrError, CapturedAudio, EngineAvailability, EngineConfidence, EngineTranscription,
        EngineUnavailable, FinalTranscription,
    };
    use std::ffi::{c_char, CStr, CString};
    use std::ptr;

    /// Status codes returned by every bridge function. They are a numbered
    /// contract with the Swift file: add at the end, never renumber.
    const STATUS_OK: i32 = 0;
    const STATUS_UNSUPPORTED_OS: i32 = 1;
    const STATUS_UNSUPPORTED_LOCALE: i32 = 2;
    const STATUS_ASSETS_MISSING: i32 = 3;
    const STATUS_FAILED: i32 = 4;
    const STATUS_UNSUPPORTED_HARDWARE: i32 = 5;

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

    /// A locale identifier as a C string. An identifier containing an interior
    /// NUL cannot be a locale, so it is reported as an unsupported one rather
    /// than being smuggled to macOS truncated.
    fn locale_argument(locale: &str) -> Result<CString, EngineUnavailable> {
        CString::new(locale).map_err(|_| EngineUnavailable::UnsupportedLocale {
            detected: locale.to_string(),
        })
    }

    /// Ask macOS whether it can transcribe `locale` right now.
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

    /// Map a non-zero status onto the boundary's vocabulary. The Swift side
    /// chooses what `detail` carries per status — a version string, a locale
    /// identifier, or a sentence — and it never contains speech.
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
            // The Mac is new enough but cannot run the on-device transcriber.
            // That is a property of this hardware, not of the build or the
            // assets, so it belongs with the other "wrong machine" answers and
            // must not offer the user an install button.
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

        // Drain every out-parameter before branching on the status, so no
        // early return can leave a bridge allocation behind.
        let text = take_string(text);
        let alternatives = take_string(alternatives);
        let detail = take_string(detail);

        match status {
            STATUS_OK => {}
            // A decode that fell over is an engine that broke, not an engine
            // that was never for this machine; the router treats the two
            // differently, so they must not share an error.
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
            // Matches the Whisper engine: leading and trailing whitespace is an
            // artefact of segment joining, not something the user said.
            transcription: FinalTranscription {
                text: text.trim().to_string(),
            },
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

// ---------------------------------------------------------------------------
// The portable path: macOS builds without the feature, and every other OS.
// ---------------------------------------------------------------------------

/// Why this build cannot reach Apple SpeechTranscriber at all.
///
/// The two cases stay distinct because they lead somewhere different: a Linux
/// user needs a different machine, while a macOS developer looking at
/// `RuntimeNotBuilt` needs `--features apple-speech-runtime`.
///
/// The branch is `cfg!` rather than `#[cfg]` on purpose: both arms then compile
/// on every platform, so a macOS developer machine type-checks the Linux and
/// Windows answer instead of leaving it to a build nobody runs until CI.
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
    // Unreachable through the trait — `transcribe` checks availability first —
    // but it has to exist and it has to be honest if it is ever called directly.
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
        // Audio, transcripts, alternatives, and confidence must reach neither
        // stdout, stderr, nor the Local Diagnostic Log (ADR-0019) — and the
        // Swift half is the easier place to forget that, because it is compiled
        // by a separate toolchain that nothing else in this repo lints. The
        // cheapest durable guard is to forbid the calls outright in both files.
        // The names are assembled at runtime so this test does not put the
        // forbidden text into the sources it scans.
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
        // Settings renders this verbatim on machines that will never run the
        // engine, so it has to read as an explanation rather than an error.
        assert_eq!(
            apple_speech_unsupported_platform(),
            EngineAvailability::Unavailable(EngineUnavailable::UnsupportedPlatform {
                detail: "Apple SpeechTranscriber is available only on macOS 26 and later"
                    .to_string(),
            })
        );
        assert!(!apple_speech_unsupported_platform().is_available());
    }

    #[test]
    fn an_unsupported_platform_offers_the_user_nothing_to_fix() {
        // A missing-assets answer earns an install button; "you are on Linux"
        // must not, or Settings would offer a dead end.
        let EngineAvailability::Unavailable(reason) = apple_speech_unsupported_platform() else {
            panic!("the engine must never be available off macOS");
        };
        assert!(!reason.is_user_resolvable());
    }

    #[test]
    fn metadata_says_the_assets_belong_to_macos() {
        // The compliance surface: Slugtale must not imply it ships, downloads,
        // or is owed credit for Apple's model.
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
    fn the_default_dictation_language_is_a_full_locale_identifier() {
        // Apple keys speech assets by locale, not by language, so a bare "en"
        // would resolve to nothing.
        let provider = AppleSpeechProvider::new();
        assert_eq!(provider.locale(), "en-US");
        assert!(provider.locale().contains('-'));

        assert_eq!(AppleSpeechProvider::for_locale("fr-FR").locale(), "fr-FR");
    }

    #[test]
    fn an_engine_that_reports_no_confidence_is_not_an_uncertain_engine() {
        // The router escalates on low confidence. Reading the bridge's "not
        // reported" sentinel as 0.0 would escalate every dictation.
        assert_eq!(reported_confidence(CONFIDENCE_UNREPORTED), None);
        assert_eq!(reported_confidence(f64::NAN), None);
        assert_eq!(reported_confidence(1.5), None);

        assert_eq!(reported_confidence(0.0), Some(0.0));
        assert_eq!(reported_confidence(1.0), Some(1.0));
        assert_eq!(reported_confidence(0.864), Some(0.864));
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
        // An engine that offered nothing must not look like one offering silence.
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
        // Any sample rate is fine otherwise: unlike Whisper, Apple's analyzer
        // converts the recording to whatever format it wants itself.
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
        // Availability is consulted on the dictation fast path; a second call
        // must not re-ask macOS about its asset inventory.
        let provider = AppleSpeechProvider::new();
        let first = provider.availability();
        assert_eq!(provider.availability(), first);
        // Refreshing is the documented way back out after an asset install.
        assert_eq!(provider.refresh_availability(), first);
    }

    #[cfg(not(all(target_os = "macos", feature = "apple-speech-runtime")))]
    #[test]
    fn a_build_that_cannot_reach_apple_speech_refuses_before_touching_the_audio() {
        // The Second Opinion router has to be able to tell "not for this
        // machine" from "this engine broke", so the refusal is an
        // EngineUnavailable rather than a Runtime error.
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

    /// The end-to-end check for the Apple OS and hardware matrix
    /// (slugtale-vjs.2). Ignored by default because it needs macOS 26, Apple
    /// silicon, and the en-US speech assets already installed — three things a
    /// build machine may not have. Run it deliberately:
    ///
    /// ```text
    /// node scripts/run-cargo.js test --lib --features apple-speech-runtime -- --ignored
    /// ```
    ///
    /// To exercise the network-denied requirement as well, run the built test
    /// binary with every socket taken away — it must still transcribe, because
    /// nothing on this path reaches for a server:
    ///
    /// ```text
    /// sandbox-exec -p '(version 1)(allow default)(deny network*)' \
    ///   target/debug/deps/slugtale_lib-* --ignored --exact \
    ///   apple_speech::tests::a_real_recording_comes_back_as_text_with_confidence
    /// ```
    #[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
    #[test]
    #[ignore = "needs macOS 26 with the en-US speech assets installed"]
    fn a_real_recording_comes_back_as_text_with_confidence() {
        let provider = AppleSpeechProvider::new();
        let availability = match provider.availability() {
            // Asking for `--ignored` is the explicit action the install flow
            // requires; macOS registers the already-downloaded assets for this
            // binary in well under a second (see the module docs).
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
        // Apple reports per-word confidence, which is the whole reason this
        // engine can be escalated *from* on a threshold rather than only on the
        // transcript anomaly rules.
        let score = result
            .confidence
            .escalation_score()
            .expect("SpeechTranscriber reports confidence when asked for it");
        assert!((0.0..=1.0).contains(&score), "confidence out of range");
        assert!(result.latency > std::time::Duration::ZERO);
    }

    /// Speak `sentence` with the system voice and return it as Captured Audio.
    ///
    /// Real speech rather than a synthetic tone: a recognizer given noise
    /// returns an empty transcript, which would make the test pass for the wrong
    /// reason. `say` and `afconvert` are macOS components invoked on the
    /// developer's own machine, exactly as the insertion rescue invokes
    /// `pbcopy` — nothing is redistributed.
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

    /// Pull the samples out of a canonical 16-bit mono WAVE file by walking its
    /// chunks, because `afconvert` is free to put metadata ahead of the audio.
    #[cfg(all(target_os = "macos", feature = "apple-speech-runtime"))]
    fn mono_16_bit_wave_samples(bytes: &[u8]) -> Vec<f32> {
        let mut cursor = 12; // past "RIFF" <size> "WAVE"
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
        // The outcome depends on the machine — OS version, hardware, which
        // languages the user has installed — so this asserts the *shape* of the
        // answer rather than a particular verdict, and that nothing in it could
        // be mistaken for speech.
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
