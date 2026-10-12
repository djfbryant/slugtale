
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
