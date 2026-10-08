//! Audio Capture (CONTEXT.md): microphone recording, the perceptual voice
//! level the dictation waveform renders, and the two sessions that drive it.
//!
//! The `DictationRecorder` and `VoiceActivationRecorder` traits (in
//! [`crate::audio_capture::recorder`]) stay the test seams, and `cpal` stays an
//! implementation detail behind `CpalAudioRecorder`. Five modules, split by
//! concern so a real-time rule is never in the same file as a session rule:
//!
//! - [`signal`] — the captured samples, in pure DSP: mono downmix, resampling,
//!   RMS level, and the digital-silence rule.
//! - [`recorder`] — the two seams, and the level callback both carry.
//! - [`ring`] — the lock-free ring the audio callback fills.
//! - [`device_policy`] — which microphone, and whether the stream already built
//!   still stands.
//! - [`cpal_recorder`] — the cpal stream and its real-time callback.
//! - [`dictation_session`] and [`listener_session`] — the two sessions.
//!
//! Everything is re-exported, so `slugtale_lib::*` call sites are unchanged.

mod cpal_recorder;
mod device_policy;
mod dictation_session;
mod listener_session;
mod recorder;
mod ring;
mod signal;

pub use cpal_recorder::CpalAudioRecorder;
pub use device_policy::{choose_input_device, MicrophoneTransport};
pub use dictation_session::{AudioCaptureOutcome, AudioCaptureSession};
pub use listener_session::VoiceActivationCapture;
pub use recorder::{AudioLevelCallback, DictationRecorder, VoiceActivationRecorder};
pub use ring::QUIET_TAIL_GUARD;
pub use signal::{
    audio_level_from_samples, captured_audio_from_interleaved_input, is_digital_silence,
    is_voice_level, voice_level_from_rms, AudioCaptureError, CapturedAudio,
    DIGITAL_SILENCE_EPSILON, VOICE_LEVEL,
};
