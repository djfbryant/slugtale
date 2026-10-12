
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
        for index in 0..target_len {
            let start = index as f64 * ratio;
            let end = start + ratio;
            resampled.push(window_average(&mono, start, end));
        }
    } else {
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

/// A denied macOS microphone does not fail the CoreAudio stream: it supplies a
/// correctly timed buffer of zeros, which Whisper canonically transcribes as
/// "You" (slugtale-d3k). Real microphones have a noise floor above this -100 dBFS
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

pub fn voice_level_from_rms(rms: f32) -> f32 {
    const NOISE_FLOOR: f32 = 0.012;
    const SPEECH_CEILING: f32 = 0.18;

    let normalized = ((rms - NOISE_FLOOR) / (SPEECH_CEILING - NOISE_FLOOR)).clamp(0.0, 1.0);
    normalized.sqrt()
}

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
                0.0, 0.0,
                0.2, 0.4,
                0.4, 0.8,
                0.8, 1.0,
                1.0, 1.0,
                0.6, 0.8,
            ],
        )
        .unwrap();

        assert_eq!(audio.sample_rate_hz, 16_000);
        assert_eq!(audio.samples.len(), 2);
        assert!((audio.samples[0] - 0.3).abs() < 1e-4);
        assert!((audio.samples[1] - 0.866_666_7).abs() < 1e-4);
    }

    #[test]
    fn downsampling_attenuates_a_nyquist_tone_instead_of_aliasing_it() {
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
        assert_eq!(voice_level_from_rms(0.0), 0.0);
        assert_eq!(voice_level_from_rms(0.005), 0.0);

        let speech = voice_level_from_rms(0.06);
        assert!(speech > 0.4, "speech should flex the wave, got {speech}");
        assert!(speech < 1.0);

        assert_eq!(voice_level_from_rms(0.18), 1.0);
        assert_eq!(voice_level_from_rms(0.9), 1.0);

        assert!(voice_level_from_rms(0.03) < voice_level_from_rms(0.06));
    }

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

        let room_floor = CapturedAudio::mono_16khz(vec![0.0002; 16_000]);

        assert!(!is_digital_silence(&room_floor.samples));
        assert!(require_captured_microphone_signal(&room_floor).is_ok());
    }

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
