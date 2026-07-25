//! NVIDIA Parakeet TDT v2 0.6B as a Transcription Engine (slugtale-vjs.1).
//!
//! Filled in by the Parakeet integration; the skeleton exists so the module
//! wiring and the Cargo feature can land with the Transcription Engine boundary.

use crate::TranscriptionEngine;

/// The engine this module provides.
pub const PARAKEET_ENGINE: TranscriptionEngine = TranscriptionEngine::Parakeet;
