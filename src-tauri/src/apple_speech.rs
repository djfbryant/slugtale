//! Apple SpeechTranscriber as a Transcription Engine (slugtale-vjs.2).
//!
//! Filled in by the Apple Speech integration; the skeleton exists so the module
//! wiring and the Cargo feature can land with the Transcription Engine boundary.

use crate::TranscriptionEngine;

/// The engine this module provides.
pub const APPLE_SPEECH_ENGINE: TranscriptionEngine = TranscriptionEngine::AppleSpeech;
