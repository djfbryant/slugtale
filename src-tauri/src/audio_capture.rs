use crate::DictationEvent;
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

    let target_len = ((mono.len() as f64) * 16_000.0 / sample_rate_hz as f64).round() as usize;
    let mut resampled = Vec::with_capacity(target_len);
    for index in 0..target_len {
        let source_position = index as f64 * sample_rate_hz as f64 / 16_000.0;
        let left = source_position.floor() as usize;
        let right = (left + 1).min(mono.len().saturating_sub(1));
        let fraction = (source_position - left as f64) as f32;
        let sample = mono[left] + (mono[right] - mono[left]) * fraction;
        resampled.push(sample);
    }

    Ok(CapturedAudio::mono_16khz(resampled))
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

pub trait AudioRecorder {
    fn start(&mut self) -> Result<(), AudioCaptureError>;
    fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError>;
    fn cancel(&mut self) -> Result<(), AudioCaptureError>;
}

pub type AudioLevelCallback = std::sync::Arc<dyn Fn(f32) + Send + Sync + 'static>;

#[derive(Default)]
pub struct CpalAudioRecorder {
    stream: Option<cpal::Stream>,
    buffer: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
    sample_rate_hz: u32,
    channels: u16,
    level_callback: Option<AudioLevelCallback>,
}

impl CpalAudioRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_level_callback(&mut self, callback: Option<AudioLevelCallback>) {
        self.level_callback = callback;
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        buffer: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
        level_callback: Option<AudioLevelCallback>,
    ) -> Result<cpal::Stream, AudioCaptureError>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        use cpal::traits::DeviceTrait;
        use cpal::Sample;

        let stream = device
            .build_input_stream(
                config.clone(),
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    let converted = data
                        .iter()
                        .copied()
                        .map(f32::from_sample)
                        .collect::<Vec<_>>();
                    if let Ok(mut samples) = buffer.try_lock() {
                        samples.extend(converted.iter().copied());
                    }
                    if let Some(callback) = &level_callback {
                        callback(voice_level_from_rms(audio_level_from_samples(&converted)));
                    }
                },
                move |error| {
                    eprintln!("audio input stream error: {error}");
                },
                None,
            )
            .map_err(|error| AudioCaptureError::new(error.to_string()))?;

        Ok(stream)
    }
}

impl AudioRecorder for CpalAudioRecorder {
    fn start(&mut self) -> Result<(), AudioCaptureError> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        self.cancel().ok();
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioCaptureError::new("no default input device is available"))?;
        let supported_config = device
            .default_input_config()
            .map_err(|error| AudioCaptureError::new(error.to_string()))?;
        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();

        self.sample_rate_hz = config.sample_rate;
        self.channels = config.channels;
        self.buffer
            .lock()
            .map_err(|_| AudioCaptureError::new("audio buffer mutex poisoned"))?
            .clear();

        let stream = match sample_format {
            cpal::SampleFormat::I8 => Self::build_stream::<i8>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::I16 => Self::build_stream::<i16>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::I32 => Self::build_stream::<i32>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::U8 => Self::build_stream::<u8>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::U16 => Self::build_stream::<u16>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::U32 => Self::build_stream::<u32>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::F32 => Self::build_stream::<f32>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            cpal::SampleFormat::F64 => Self::build_stream::<f64>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_callback.clone(),
            ),
            other => {
                return Err(AudioCaptureError::new(format!(
                    "unsupported input sample format: {other}"
                )))
            }
        }?;

        stream
            .play()
            .map_err(|error| AudioCaptureError::new(error.to_string()))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        self.stream.take();
        let samples = {
            let mut guard = self
                .buffer
                .lock()
                .map_err(|_| AudioCaptureError::new("audio buffer mutex poisoned"))?;
            std::mem::take(&mut *guard)
        };

        captured_audio_from_interleaved_input(self.sample_rate_hz, self.channels, &samples)
    }

    fn cancel(&mut self) -> Result<(), AudioCaptureError> {
        self.stream.take();
        self.buffer
            .lock()
            .map_err(|_| AudioCaptureError::new("audio buffer mutex poisoned"))?
            .clear();
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AudioCaptureOutcome {
    Completed(CapturedAudio),
    Discarded,
}

pub struct AudioCaptureSession<R> {
    recorder: R,
    active: bool,
}

impl<R> AudioCaptureSession<R>
where
    R: AudioRecorder,
{
    pub fn new(recorder: R) -> Self {
        Self {
            recorder,
            active: false,
        }
    }

    pub fn on_event(
        &mut self,
        event: DictationEvent,
    ) -> Result<Option<AudioCaptureOutcome>, AudioCaptureError> {
        match event {
            DictationEvent::Start => {
                self.recorder.start()?;
                self.active = true;
                Ok(None)
            }
            DictationEvent::Stop if self.active => {
                self.active = false;
                Ok(Some(AudioCaptureOutcome::Completed(self.recorder.stop()?)))
            }
            DictationEvent::Cancel if self.active => {
                self.active = false;
                self.recorder.cancel()?;
                Ok(Some(AudioCaptureOutcome::Discarded))
            }
            DictationEvent::Stop | DictationEvent::Cancel => Ok(None),
        }
    }

    pub fn recorder(&self) -> &R {
        &self.recorder
    }

    pub fn recorder_mut(&mut self) -> &mut R {
        &mut self.recorder
    }
}

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
        assert_eq!(audio.samples, vec![0.0, 0.9]);
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

    #[test]
    fn audio_capture_session_stops_with_captured_samples_for_transcription() {
        let recorder = FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.0, 0.2, -0.2]));
        let mut session = AudioCaptureSession::new(recorder);

        assert_eq!(session.on_event(DictationEvent::Start).unwrap(), None);
        let completed = session.on_event(DictationEvent::Stop).unwrap();

        assert_eq!(
            completed,
            Some(AudioCaptureOutcome::Completed(CapturedAudio::mono_16khz(
                vec![0.0, 0.2, -0.2]
            )))
        );
        assert_eq!(
            session.recorder().events.borrow().as_slice(),
            &["start", "stop"]
        );
    }

    #[test]
    fn audio_capture_session_cancel_discards_without_returning_audio() {
        let recorder = FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.4, 0.5]));
        let mut session = AudioCaptureSession::new(recorder);

        session.on_event(DictationEvent::Start).unwrap();
        let discarded = session.on_event(DictationEvent::Cancel).unwrap();

        assert_eq!(discarded, Some(AudioCaptureOutcome::Discarded));
        assert_eq!(
            session.recorder().events.borrow().as_slice(),
            &["start", "cancel"]
        );
    }

    struct FakeAudioRecorder {
        audio: CapturedAudio,
        events: std::cell::RefCell<Vec<&'static str>>,
    }

    impl FakeAudioRecorder {
        fn new(audio: CapturedAudio) -> Self {
            Self {
                audio,
                events: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl AudioRecorder for FakeAudioRecorder {
        fn start(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push("start");
            Ok(())
        }

        fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            self.events.borrow_mut().push("stop");
            Ok(self.audio.clone())
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push("cancel");
            Ok(())
        }
    }
}
