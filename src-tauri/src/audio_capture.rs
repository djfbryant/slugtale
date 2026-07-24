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

fn require_captured_microphone_signal(audio: &CapturedAudio) -> Result<(), AudioCaptureError> {
    // A denied macOS microphone does not fail the CoreAudio stream. It supplies
    // a correctly timed buffer of digital silence instead, which Whisper
    // canonically transcribes as "You" (slugtale-d3k). Real microphones have a
    // noise floor above this -100 dBFS threshold even in a quiet room.
    const DIGITAL_SILENCE_EPSILON: f32 = 0.000_01;

    let rms = audio_level_from_samples(&audio.samples);
    let peak = audio
        .samples
        .iter()
        .fold(0.0f32, |highest, sample| highest.max(sample.abs()));
    if rms <= DIGITAL_SILENCE_EPSILON && peak <= DIGITAL_SILENCE_EPSILON {
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

/// Publishes the dictation waveform level from the audio callback to a
/// dedicated emitter thread. The audio callback must stay real-time safe — a
/// Tauri `emit` (IPC into the webview) from inside it stalls the ALSA/PipeWire
/// period and the driver drops capture buffers, garbling transcription
/// (slugtale-65l) — so the callback only stores the latest level in an atomic
/// and the emitter thread forwards it at a UI cadence.
struct LevelEmitter {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl LevelEmitter {
    const EMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(33);

    fn spawn(
        level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
        callback: AudioLevelCallback,
    ) -> std::io::Result<Self> {
        use std::sync::atomic::Ordering;

        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let thread_running = running.clone();
        let handle = std::thread::Builder::new()
            .name("slugtale-audio-level".to_string())
            .spawn(move || {
                while thread_running.load(Ordering::Relaxed) {
                    callback(f32::from_bits(level_bits.load(Ordering::Relaxed)));
                    std::thread::sleep(Self::EMIT_INTERVAL);
                }
            })?;
        Ok(Self { running, handle })
    }

    /// Stop and join the emitter so no stale level is emitted after the
    /// recording ends (the Tauri layer resets the waveform to zero right after).
    fn stop(self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

#[derive(Default)]
pub struct CpalAudioRecorder {
    stream: Option<cpal::Stream>,
    buffer: std::sync::Arc<std::sync::Mutex<Vec<f32>>>,
    level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
    level_emitter: Option<LevelEmitter>,
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
        level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
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
                    // Real-time audio callback: no allocation, no IPC, and no
                    // dropped chunks. A blocking `lock` is safe here — the only
                    // other holders (start's clear, stop's drain) run outside
                    // active capture and hold it briefly — whereas the previous
                    // `try_lock` silently discarded whole chunks under
                    // contention, leaving gaps in the dictation (slugtale-65l).
                    let mut sum_of_squares = 0.0f32;
                    if let Ok(mut samples) = buffer.lock() {
                        samples.reserve(data.len());
                        for value in data.iter().copied() {
                            let sample = f32::from_sample(value);
                            sum_of_squares += sample.clamp(-1.0, 1.0).powi(2);
                            samples.push(sample);
                        }
                    }
                    if !data.is_empty() {
                        let rms = (sum_of_squares / data.len() as f32).sqrt().clamp(0.0, 1.0);
                        level_bits.store(
                            voice_level_from_rms(rms).to_bits(),
                            std::sync::atomic::Ordering::Relaxed,
                        );
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

    fn stop_level_emitter(&mut self) {
        if let Some(emitter) = self.level_emitter.take() {
            emitter.stop();
        }
        self.level_bits
            .store(0f32.to_bits(), std::sync::atomic::Ordering::Relaxed);
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

        self.level_bits
            .store(0f32.to_bits(), std::sync::atomic::Ordering::Relaxed);
        let stream = match sample_format {
            cpal::SampleFormat::I8 => Self::build_stream::<i8>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::I16 => Self::build_stream::<i16>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::I32 => Self::build_stream::<i32>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::U8 => Self::build_stream::<u8>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::U16 => Self::build_stream::<u16>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::U32 => Self::build_stream::<u32>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::F32 => Self::build_stream::<f32>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
            ),
            cpal::SampleFormat::F64 => Self::build_stream::<f64>(
                &device,
                &config,
                self.buffer.clone(),
                self.level_bits.clone(),
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

        if let Some(callback) = self.level_callback.clone() {
            match LevelEmitter::spawn(self.level_bits.clone(), callback) {
                Ok(emitter) => self.level_emitter = Some(emitter),
                // The waveform is cosmetic; capture must not fail without it.
                Err(error) => eprintln!("could not start audio level emitter: {error}"),
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        self.stream.take();
        self.stop_level_emitter();
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
        self.stop_level_emitter();
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
                let audio = self.recorder.stop()?;
                require_captured_microphone_signal(&audio)?;
                Ok(Some(AudioCaptureOutcome::Completed(audio)))
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
    fn audio_capture_session_rejects_digital_silence_before_transcription() {
        let recorder = FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.0; 80_000]));
        let mut session = AudioCaptureSession::new(recorder);

        session.on_event(DictationEvent::Start).unwrap();
        let error = session.on_event(DictationEvent::Stop).unwrap_err();

        assert_eq!(
            error,
            AudioCaptureError::new(
                "no microphone signal was captured; check Slugtale under System Settings > Privacy & Security > Microphone"
            )
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
