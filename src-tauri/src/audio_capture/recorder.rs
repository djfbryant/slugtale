//! The two microphone seams, and the level publisher they share.
//!
//! [`DictationRecorder`] is what a dictation drives: prepare ahead of the first
//! Hotkey, record, end, and cut a Dictation Segment off it at a watermark.
//! [`VoiceActivationRecorder`] is the smaller half the always-listening
//! microphone uses — start, cancel, and take a chunk of what it has already
//! heard — so no test double pays for methods it never calls.
//!
//! These traits are the boundary the rest of the app and every test double
//! speak. The cpal implementation lives in
//! [`crate::audio_capture::cpal_recorder`], and which module owns it is an
//! implementation detail.

use crate::{AudioCaptureError, CapturedAudio};
use std::sync::Arc;

pub type AudioLevelCallback = std::sync::Arc<dyn Fn(f32) + Send + Sync + 'static>;

/// The microphone a Dictation drives: prepare it ahead of the first Hotkey,
/// record, end, and cut a Dictation Segment off it at a watermark.
pub trait DictationRecorder {
    /// Do the safe part of starting capture ahead of the first Hotkey.
    ///
    /// Safe means: discover and validate the default input device and format,
    /// allocate the capture ring, and build the input stream in a stopped
    /// state. A stopped stream does not activate the microphone — proved on
    /// real macOS by `examples/mic_indicator_probe.rs`: the device's
    /// `kAudioDevicePropertyDeviceIsRunningSomewhere` property stays false
    /// until the stream plays, and the microphone indicator follows that same
    /// running state. Preparation never requests the microphone permission;
    /// callers must only prepare when permission is already granted, so a
    /// denied user is never prompted from idle time. Idempotent: preparing
    /// twice prepares once, and preparing while recording changes nothing. A
    /// failed prepare may be retried; Start never requires it.
    fn prepare(&mut self) -> Result<(), AudioCaptureError>;

    fn start(&mut self) -> Result<(), AudioCaptureError>;
    fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError>;
    fn cancel(&mut self) -> Result<(), AudioCaptureError>;

    /// Take the audio captured so far and leave the microphone running, so a
    /// Segment Pause can be transcribed and inserted while the user carries on
    /// dictating (CONTEXT.md: Dictation Segment).
    ///
    /// Only audio through `cut` plus the module's documented quiet-tail guard
    /// ([`QUIET_TAIL_GUARD`]) leaves the ring. `cut` is a stable watermark —
    /// the ring position of the last voiced sample when the Pause Flush was
    /// queued — so a slow worker cannot append later speech or extra silence to
    /// this segment (slugtale-g1o.4).
    ///
    /// It must not drop a single sample: whatever arrives while the returned
    /// segment is decoding belongs to the next one.
    fn cut_segment(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError>;

    /// The cell holding the ring position of the most recent voiced sample —
    /// the watermark a queued Pause Flush should carry as its cut. `0` when
    /// nothing voiced has been captured since the ring was cleared.
    ///
    /// A shared cell rather than a locked read, because the level-emitter thread
    /// asks for it while Stop or Cancel may hold this recorder's lock and be
    /// joining that very thread: a locked read there leaves two threads waiting
    /// for each other, and the app freezes at a pause boundary (slugtale-xqzs).
    /// The cell belongs to the recorder for its whole life, so a caller may hold
    /// it and read the watermark without touching the recorder at all.
    fn voice_watermark_cell(&self) -> Arc<std::sync::atomic::AtomicU64>;

    /// Install the real-time-safe level publisher (see [`AudioLevelCallback`]).
    /// Only backends with a live audio callback distribute levels; the default
    /// records nothing.
    fn set_level_callback(&mut self, _callback: Option<AudioLevelCallback>) {}

    /// Whether to record from the built-in microphone when the default one is
    /// Bluetooth (see [`choose_input_device`]). Read by the next `prepare` or
    /// `start`; only backends that pick a real device act on it.
    fn set_prefer_built_in_microphone(&mut self, _prefer: bool) {}
}

/// The microphone the Voice Activation listener drives. The listener is always
/// on and asks only for a chunk of what it has already heard, to run a wake
/// check on. It never prepares, never ends a recording, and never cuts a
/// Dictation Segment, so it asks for none of the Dictation Session's half and no
/// test double pays for methods it never calls.
pub trait VoiceActivationRecorder {
    fn start(&mut self) -> Result<(), AudioCaptureError>;
    fn cancel(&mut self) -> Result<(), AudioCaptureError>;
    fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError>;
}
