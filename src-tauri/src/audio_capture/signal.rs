//! The captured microphone signal: normalizing a device's samples into the one
//! shape an engine takes, and the two readings of it the rest of the app asks
//! for — how loud it is, and whether it is silence at all.
//!
//! Everything here is pure. No device, no thread, no allocation beyond the
//! answer, which is why every rule the app relies on (the digital-silence guard
//! that catches a denied microphone, the perceptual level the waveform renders,
//! and the box filter that stops 48 kHz audio aliasing into the speech band) is
//! decided here and only here.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedAudio {
    pub sample_rate_hz: u32,
    pub samples: Vec<f32>,
}

impl CapturedAudio {
    pub fn mono_16khz(samples: Vec<f32>) -> Self {
        Self {
            sample_rate_hz: 16_000,
            samples,
        }
    }
}

pub fn captured_audio_from_interleaved_input(
    sample_rate_hz: u32,
    channels: u16,
    samples: &[f32],
) -> Result<CapturedAudio, AudioCaptureError> {
    if sample_rate_hz == 0 {
        return Err(AudioCaptureError::new("input sample rate must be non-zero"));
    }
    if channels == 0 {
        return Err(AudioCaptureError::new(
            "input channel count must be non-zero",
        ));
    }

    let channels = channels as usize;
    let mut mono = Vec::with_capacity(samples.len() / channels);
    for frame in samples.chunks_exact(channels) {
        mono.push(frame.iter().copied().sum::<f32>() / channels as f32);
    }

    if sample_rate_hz == 16_000 {
        return Ok(CapturedAudio::mono_16khz(mono));
    }

    let ratio = sample_rate_hz as f64 / 16_000.0;
    let target_len = ((mono.len() as f64) / ratio).round() as usize;
    let mut resampled = Vec::with_capacity(target_len);

    if sample_rate_hz > 16_000 {
        // Downsampling. Average each output sample over its whole source window
        // (a box filter) so content above the 8 kHz Nyquist limit is band-limited
        // away instead of aliasing into the speech band. Point/linear sampling
        // here folds high-frequency mic content down as noise and garbles Whisper
        // on 44.1/48 kHz mics (slugtale-8dj).
        for index in 0..target_len {
            let start = index as f64 * ratio;
            let end = start + ratio;
            resampled.push(window_average(&mono, start, end));
        }
    } else {
        // Upsampling (sub-16 kHz mics, rare). Linear interpolation is smooth and
        // adds no aliasing when moving to a higher rate.
        for index in 0..target_len {
            let source_position = index as f64 * ratio;
            let left = source_position.floor() as usize;
            let right = (left + 1).min(mono.len().saturating_sub(1));
            let fraction = (source_position - left as f64) as f32;
            let sample = mono[left] + (mono[right] - mono[left]) * fraction;
            resampled.push(sample);
        }
    }

    Ok(CapturedAudio::mono_16khz(resampled))
}

/// Average the mono signal over the source-sample window `[start, end)`,
/// treating each input sample as covering a unit-width cell. This box filter
/// band-limits the signal ahead of decimation so downsampling to 16 kHz does not
/// alias high-frequency microphone content into the speech band.
fn window_average(mono: &[f32], start: f64, end: f64) -> f32 {
    let len = mono.len();
    if len == 0 {
        return 0.0;
    }
    let start = start.max(0.0);
    let end = end.min(len as f64);
    if end <= start {
        return mono[(start as usize).min(len - 1)];
    }

    let first = start.floor() as usize;
    let last = (end.ceil() as usize).min(len);
    let mut weighted = 0.0f64;
    let mut total = 0.0f64;
    for (offset, &value) in mono[first..last].iter().enumerate() {
        let cell_start = (first + offset) as f64;
        let cell_end = cell_start + 1.0;
        let overlap = cell_end.min(end) - cell_start.max(start);
        if overlap > 0.0 {
            weighted += value as f64 * overlap;
            total += overlap;
        }
    }

    if total > 0.0 {
        (weighted / total) as f32
    } else {
        mono[first.min(len - 1)]
    }
}

pub fn audio_level_from_samples(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let mean_square = samples
        .iter()
        .map(|sample| sample.clamp(-1.0, 1.0).powi(2))
        .sum::<f32>()
        / samples.len() as f32;
    mean_square.sqrt().clamp(0.0, 1.0)
}

pub const DIGITAL_SILENCE_EPSILON: f32 = 0.000_01;

/// Whether a captured buffer is digital silence rather than a quiet room.
///
/// A denied macOS microphone does not fail the CoreAudio stream: it supplies a
/// correctly timed buffer of zeros, which Whisper canonically transcribes as
/// "You" (slugtale-d3k). Real microphones have a noise floor above this -100 dBFS
/// threshold even in a quiet room, so the rule needs both terms: a buffer is
/// silence only when it is silent by RMS *and* by peak.
///
/// This is the one copy of that rule. The Dictation Session refuses a silent
/// recording with it; the Voice Activation window tells digital silence apart
/// from a quiet room with it. Written twice, the two copies were free to differ
/// on `&&` against `||`, and did.
pub fn is_digital_silence(samples: &[f32]) -> bool {
    let rms = audio_level_from_samples(samples);
    let peak = samples
        .iter()
        .fold(0.0f32, |highest, sample| highest.max(sample.abs()));
    rms <= DIGITAL_SILENCE_EPSILON && peak <= DIGITAL_SILENCE_EPSILON
}

pub(super) fn require_captured_microphone_signal(
    audio: &CapturedAudio,
) -> Result<(), AudioCaptureError> {
    if is_digital_silence(&audio.samples) {
        return Err(AudioCaptureError::new(
            "no microphone signal was captured; check Slugtale under System Settings > Privacy & Security > Microphone",
        ));
    }

    Ok(())
}

/// Map a raw microphone RMS level into the 0..1 range the dictation waveform
/// renders. Raw speech RMS is tiny (~0.06) and barely moves the bars, so the
/// waveform looked like it drifted on its own rather than reacting to the voice
/// (slugtale-hla). A noise floor keeps quiet rooms in the idle state, a ceiling
/// saturates loud speech, and a square-root curve lifts ordinary speech into a
/// clearly active, bouncing range.
pub fn voice_level_from_rms(rms: f32) -> f32 {
    const NOISE_FLOOR: f32 = 0.012;
    const SPEECH_CEILING: f32 = 0.18;

    let normalized = ((rms - NOISE_FLOOR) / (SPEECH_CEILING - NOISE_FLOOR)).clamp(0.0, 1.0);
    normalized.sqrt()
}

/// The perceptual voice level above which the user counts as speaking.
///
/// This is the Dictation Bar's own `VOICE_LEVEL` (src/dictation-bar.html), and
/// the two must stay the same number: the bar visibly flexes its waveform on
/// exactly the input that keeps a Segment Pause from firing, so a user watching
/// the bar can see why a flush did or did not happen. `tests/frontend-seam.test.mjs`
/// reads both literals and fails if they drift.
///
/// Known and deliberately unchanged: the bar holds "voice" for
/// `VOICE_HOLD_MS = 700` after the level drops, while this answer and the
/// capture ring's watermark are instantaneous. For 700 ms the bar reads voice
/// after both Rust consumers have moved on. Closing the gap means changing what
/// the bar does, which is a product decision, not a refactor (slugtale-hdef.12).
pub const VOICE_LEVEL: f32 = 0.08;

/// Whether a voice level counts as speech. Strictly above, so a level sitting
/// exactly on the threshold is room noise to the bar and here alike; treating it
/// as voice would let a steady hum silently disable flushing.
pub fn is_voice_level(level: f32) -> bool {
    level > VOICE_LEVEL
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioCaptureError {
    message: String,
}

impl AudioCaptureError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AudioCaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "audio capture failed: {}", self.message)
    }
}

impl std::error::Error for AudioCaptureError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_audio_is_normalized_to_mono_16khz_samples() {
        let audio = captured_audio_from_interleaved_input(
            48_000,
            2,
            &[
                0.0, 0.0, // mono frame 0.0
                0.2, 0.4, // mono frame 0.3
                0.4, 0.8, // mono frame 0.6
                0.8, 1.0, // mono frame 0.9
                1.0, 1.0, // mono frame 1.0
                0.6, 0.8, // mono frame 0.7
            ],
        )
        .unwrap();

        assert_eq!(audio.sample_rate_hz, 16_000);
        // 48 kHz -> 16 kHz is a 3x downsample: each output sample is the average
        // of its three-sample source window (band-limiting), not every third
        // sample. Window [0,3) = mean(0.0, 0.3, 0.6); window [3,6) = mean(0.9,
        // 1.0, 0.7).
        assert_eq!(audio.samples.len(), 2);
        assert!((audio.samples[0] - 0.3).abs() < 1e-4);
        assert!((audio.samples[1] - 0.866_666_7).abs() < 1e-4);
    }

    #[test]
    fn downsampling_attenuates_a_nyquist_tone_instead_of_aliasing_it() {
        // A full-amplitude tone at the input Nyquist frequency (here 16 kHz in a
        // 32 kHz signal, the alternating +1/-1 sequence) is above the 8 kHz
        // Nyquist limit of the 16 kHz target. Without a band-limiting filter it
        // aliases straight into the speech band at full amplitude; with one it is
        // averaged away. This is the fricative/sibilant garble that made 48 kHz
        // mic dictation far worse than macOS 16 kHz-native capture (slugtale-8dj).
        let audio = captured_audio_from_interleaved_input(
            32_000,
            1,
            &[1.0, -1.0, 1.0, -1.0, 1.0, -1.0, 1.0, -1.0],
        )
        .unwrap();

        assert_eq!(audio.sample_rate_hz, 16_000);
        for sample in &audio.samples {
            assert!(
                sample.abs() < 0.1,
                "Nyquist tone should be attenuated, got {sample}"
            );
        }
    }

    #[test]
    fn audio_level_reports_clamped_rms_for_voice_feedback() {
        assert_eq!(audio_level_from_samples(&[]), 0.0);
        assert_eq!(audio_level_from_samples(&[2.0]), 1.0);
        assert!((audio_level_from_samples(&[0.0, 0.5, -0.5]) - 0.408).abs() < 0.001);
    }

    #[test]
    fn voice_level_maps_speech_rms_into_a_visibly_responsive_range() {
        // Silence and quiet room noise stay below the idle threshold so the bar
        // shows only its subtle listening state, not a false "active" flex.
        assert_eq!(voice_level_from_rms(0.0), 0.0);
        assert_eq!(voice_level_from_rms(0.005), 0.0);

        // Ordinary speech (raw RMS ~0.06) is faint on its own but must drive a
        // clearly active waveform — comfortably past the frontend's 0.08 gate.
        let speech = voice_level_from_rms(0.06);
        assert!(speech > 0.4, "speech should flex the wave, got {speech}");
        assert!(speech < 1.0);

        // Loud speech saturates and stays clamped rather than overshooting.
        assert_eq!(voice_level_from_rms(0.18), 1.0);
        assert_eq!(voice_level_from_rms(0.9), 1.0);

        // The mapping is monotonic: louder input never renders as a smaller bar.
        assert!(voice_level_from_rms(0.03) < voice_level_from_rms(0.06));
    }

    /// The one rule both a denied microphone and the Voice Activation window
    /// ask, classified from both sides. A quiet room must not read as silence:
    /// if it did, a room with a fan in it would be told its microphone was
    /// denied.
    #[test]
    fn digital_silence_is_a_denied_microphone_and_a_quiet_room_is_not() {
        let denied = CapturedAudio::mono_16khz(vec![0.0; 16_000]);

        assert!(
            is_digital_silence(&denied.samples),
            "a denied macOS microphone hands over a correctly timed buffer of zeros"
        );
        assert_eq!(
            require_captured_microphone_signal(&denied).unwrap_err(),
            AudioCaptureError::new(
                "no microphone signal was captured; check Slugtale under System Settings > Privacy & Security > Microphone"
            )
        );

        // A quiet room is well under speech but far above the -100 dBFS
        // threshold, on both the RMS term and the peak term.
        let room_floor = CapturedAudio::mono_16khz(vec![0.0002; 16_000]);

        assert!(!is_digital_silence(&room_floor.samples));
        assert!(require_captured_microphone_signal(&room_floor).is_ok());
    }

    /// The rule needs both terms, and the two can genuinely disagree: a buffer
    /// can be silent by RMS and loud by peak. Ten samples of 0.001 in ten
    /// seconds of silence give an RMS of about 7.9e-6, under the 1e-5
    /// threshold, against a peak of 0.001, far over it. That is one desk click
    /// in a recording with no speech, and it is not a denied microphone. Swap
    /// the `&&` for a `||` and this is silence.
    #[test]
    fn a_quiet_buffer_with_one_loud_sample_is_not_digital_silence() {
        let mut click = vec![0.0; 160_000];
        for sample in click.iter_mut().step_by(16_000) {
            *sample = 0.001;
        }

        assert!(audio_level_from_samples(&click) <= DIGITAL_SILENCE_EPSILON);
        assert!(!is_digital_silence(&click));
        assert!(require_captured_microphone_signal(&CapturedAudio::mono_16khz(click)).is_ok());
    }

    #[test]
    fn upsampling_doubles_sub_16khz_mono_input() {
        let audio = captured_audio_from_interleaved_input(8_000, 1, &[0.0, 1.0, 0.0, 1.0]).unwrap();

        assert_eq!(audio.sample_rate_hz, 16_000);
        assert_eq!(audio.samples.len(), 8);
        assert!((audio.samples[0] - 0.0).abs() < 1e-4);
        assert!((audio.samples[2] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn zero_sample_rate_input_is_rejected() {
        let error = captured_audio_from_interleaved_input(0, 1, &[0.0]).unwrap_err();
        assert!(error.to_string().contains("sample rate"));
    }

    #[test]
    fn zero_channel_input_is_rejected() {
        let error = captured_audio_from_interleaved_input(16_000, 0, &[0.0]).unwrap_err();
        assert!(error.to_string().contains("channel"));
    }
}
