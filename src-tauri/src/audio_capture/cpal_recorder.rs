
use crate::audio_capture::device_policy::{
    paused_stream_is_reusable, prepare_state_after, recording_device, should_attempt_prepare,
    InputStreamIdentity, PrepareState,
};
use crate::audio_capture::recorder::{
    AudioLevelCallback, DictationRecorder, VoiceActivationRecorder,
};
use crate::audio_capture::ring::{RealtimeCaptureBuffer, QUIET_TAIL_GUARD};
use crate::audio_capture::signal::captured_audio_from_interleaved_input;
use crate::{is_voice_level, voice_level_from_rms, AudioCaptureError, CapturedAudio};
use std::sync::Arc;

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

    fn stop(self) {
        self.running
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

type StreamBuilder =
    fn(
        &cpal::Device,
        &cpal::StreamConfig,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<(cpal::Stream, std::sync::Arc<RealtimeCaptureBuffer>), AudioCaptureError>;

const INPUT_STREAM_BUILDERS: &[(cpal::SampleFormat, StreamBuilder)] = &[
    (
        cpal::SampleFormat::I8,
        CpalAudioRecorder::build_stream::<i8>,
    ),
    (
        cpal::SampleFormat::I16,
        CpalAudioRecorder::build_stream::<i16>,
    ),
    (
        cpal::SampleFormat::I32,
        CpalAudioRecorder::build_stream::<i32>,
    ),
    (
        cpal::SampleFormat::U8,
        CpalAudioRecorder::build_stream::<u8>,
    ),
    (
        cpal::SampleFormat::U16,
        CpalAudioRecorder::build_stream::<u16>,
    ),
    (
        cpal::SampleFormat::U32,
        CpalAudioRecorder::build_stream::<u32>,
    ),
    (
        cpal::SampleFormat::F32,
        CpalAudioRecorder::build_stream::<f32>,
    ),
    (
        cpal::SampleFormat::F64,
        CpalAudioRecorder::build_stream::<f64>,
    ),
];

fn stream_builder_for(sample_format: cpal::SampleFormat) -> Option<StreamBuilder> {
    INPUT_STREAM_BUILDERS
        .iter()
        .find(|(format, _)| *format == sample_format)
        .map(|(_, builder)| *builder)
}

#[derive(Default)]
pub struct CpalAudioRecorder {
    stream: Option<cpal::Stream>,
    stream_identity: Option<InputStreamIdentity>,
    stream_active: bool,
    buffer: Option<std::sync::Arc<RealtimeCaptureBuffer>>,
    voice_watermark: std::sync::Arc<std::sync::atomic::AtomicU64>,
    prepare_state: PrepareState,
    level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
    level_emitter: Option<LevelEmitter>,
    sample_rate_hz: u32,
    channels: u16,
    level_callback: Option<AudioLevelCallback>,
    prefer_built_in_microphone: bool,
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
        level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
        voice_watermark: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<(cpal::Stream, std::sync::Arc<RealtimeCaptureBuffer>), AudioCaptureError>
    where
        T: cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        use cpal::traits::DeviceTrait;
        use cpal::Sample;

        let channels = usize::from(config.channels);
        if channels == 0 {
            return Err(AudioCaptureError::new(
                "input channel count must be non-zero",
            ));
        }
        let buffer = std::sync::Arc::new(RealtimeCaptureBuffer::for_sample_rate(
            config.sample_rate,
            voice_watermark,
        )?);
        let callback_buffer = buffer.clone();
        let stream = device
            .build_input_stream(
                *config,
                move |data: &[T], _: &cpal::InputCallbackInfo| {
                    let mut sum_of_squares = 0.0f32;
                    for frame in data.chunks_exact(channels) {
                        let mut mono_sum = 0.0f32;
                        for value in frame.iter().copied() {
                            let sample = f32::from_sample(value);
                            sum_of_squares += sample.clamp(-1.0, 1.0).powi(2);
                            mono_sum += sample;
                        }
                        callback_buffer.push_sample(mono_sum / channels as f32);
                    }
                    if !data.is_empty() {
                        let rms = (sum_of_squares / data.len() as f32).sqrt().clamp(0.0, 1.0);
                        let level = voice_level_from_rms(rms);
                        if is_voice_level(level) {
                            callback_buffer.mark_voice();
                        }
                        level_bits.store(level.to_bits(), std::sync::atomic::Ordering::Relaxed);
                    }
                },
                move |error| {
                    eprintln!("audio input stream error: {error}");
                },
                None,
            )
            .map_err(|error| AudioCaptureError::new(error.to_string()))?;

        Ok((stream, buffer))
    }

    fn build_stream_for_format(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        sample_format: cpal::SampleFormat,
        level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
        voice_watermark: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<(cpal::Stream, std::sync::Arc<RealtimeCaptureBuffer>), AudioCaptureError> {
        let builder = stream_builder_for(sample_format).ok_or_else(|| {
            AudioCaptureError::new(format!("unsupported input sample format: {sample_format}"))
        })?;
        builder(device, config, level_bits, voice_watermark)
    }

    fn install_stream(
        &mut self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        identity: &InputStreamIdentity,
    ) -> Result<(), AudioCaptureError> {
        use std::sync::atomic::Ordering;

        self.stream.take();
        self.buffer = None;
        self.voice_watermark.store(0, Ordering::Release);
        let (stream, buffer) = Self::build_stream_for_format(
            device,
            config,
            identity.sample_format,
            self.level_bits.clone(),
            std::sync::Arc::clone(&self.voice_watermark),
        )?;
        self.stream = Some(stream);
        self.buffer = Some(buffer);
        self.stream_identity = Some(identity.clone());
        Ok(())
    }

    fn pause_active_stream(&mut self) {
        use cpal::traits::StreamTrait;

        if !self.stream_active {
            return;
        }

        if let Some(stream) = self.stream.as_ref() {
            if let Err(error) = stream.pause() {
                eprintln!("could not pause audio input stream; rebuilding next time: {error}");
                self.stream.take();
                self.stream_identity = None;
            }
        }
        self.stream_active = false;
    }

    fn stop_level_emitter(&mut self) {
        if let Some(emitter) = self.level_emitter.take() {
            emitter.stop();
        }
        self.level_bits
            .store(0f32.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

impl DictationRecorder for CpalAudioRecorder {
    fn set_level_callback(&mut self, callback: Option<AudioLevelCallback>) {
        CpalAudioRecorder::set_level_callback(self, callback);
    }

    fn set_prefer_built_in_microphone(&mut self, prefer: bool) {
        self.prefer_built_in_microphone = prefer;
    }

    fn prepare(&mut self) -> Result<(), AudioCaptureError> {
        use cpal::traits::DeviceTrait;

        if !should_attempt_prepare(&self.prepare_state, self.stream.is_some()) {
            return Ok(());
        }

        let prepared = (|| -> Result<InputStreamIdentity, AudioCaptureError> {
            let device = recording_device(&cpal::default_host(), self.prefer_built_in_microphone)?;
            let supported_config = device
                .default_input_config()
                .map_err(|error| AudioCaptureError::new(error.to_string()))?;
            let sample_format = supported_config.sample_format();
            let config: cpal::StreamConfig = supported_config.into();
            let identity = InputStreamIdentity {
                device_id: device.id().ok(),
                sample_format,
                sample_rate_hz: config.sample_rate,
                channels: config.channels,
            };

            self.sample_rate_hz = config.sample_rate;
            self.channels = 1;

            self.install_stream(&device, &config, &identity)?;

            Ok(identity)
        })();

        self.prepare_state = prepare_state_after(prepared.as_ref());
        prepared.map(|_| ())
    }

    fn start(&mut self) -> Result<(), AudioCaptureError> {
        use cpal::traits::{DeviceTrait, StreamTrait};

        self.pause_active_stream();
        self.stop_level_emitter();

        let device = recording_device(&cpal::default_host(), self.prefer_built_in_microphone)?;
        let supported_config = device
            .default_input_config()
            .map_err(|error| AudioCaptureError::new(error.to_string()))?;
        let sample_format = supported_config.sample_format();
        let config: cpal::StreamConfig = supported_config.into();
        let identity = InputStreamIdentity {
            device_id: device.id().ok(),
            sample_format,
            sample_rate_hz: config.sample_rate,
            channels: config.channels,
        };

        self.sample_rate_hz = config.sample_rate;
        self.channels = 1;
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.clear();
        }

        self.level_bits
            .store(0f32.to_bits(), std::sync::atomic::Ordering::Relaxed);

        let reused_stream = paused_stream_is_reusable(
            self.stream.is_some(),
            self.buffer.is_some(),
            self.stream_identity.as_ref(),
            &identity,
        );
        if !reused_stream {
            self.install_stream(&device, &config, &identity)?;
        }

        if let Err(error) = self.stream.as_ref().expect("audio stream exists").play() {
            if !reused_stream {
                return Err(AudioCaptureError::new(error.to_string()));
            }

            self.install_stream(&device, &config, &identity)?;
            self.stream
                .as_ref()
                .expect("rebuilt audio stream exists")
                .play()
                .map_err(|error| AudioCaptureError::new(error.to_string()))?;
        }
        self.stream_active = true;
        self.prepare_state = PrepareState::Prepared { identity };

        if let Some(callback) = self.level_callback.clone() {
            match LevelEmitter::spawn(self.level_bits.clone(), callback) {
                Ok(emitter) => self.level_emitter = Some(emitter),
                Err(error) => eprintln!("could not start audio level emitter: {error}"),
            }
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        self.pause_active_stream();
        self.stop_level_emitter();
        let samples = self
            .buffer
            .as_ref()
            .ok_or_else(|| AudioCaptureError::new("audio capture buffer is unavailable"))?
            .drain()?;

        captured_audio_from_interleaved_input(self.sample_rate_hz, self.channels, &samples)
    }

    fn cancel(&mut self) -> Result<(), AudioCaptureError> {
        self.pause_active_stream();
        self.stop_level_emitter();
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.clear();
        }
        Ok(())
    }

    fn cut_segment(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
        let buffer = self
            .buffer
            .as_ref()
            .ok_or_else(|| AudioCaptureError::new("audio capture buffer is unavailable"))?;
        let guard = (QUIET_TAIL_GUARD.as_secs_f64() * f64::from(self.sample_rate_hz)) as u64;
        let samples = buffer.drain_through(cut, guard)?;

        captured_audio_from_interleaved_input(self.sample_rate_hz, self.channels, &samples)
    }

    fn voice_watermark_cell(&self) -> std::sync::Arc<std::sync::atomic::AtomicU64> {
        std::sync::Arc::clone(&self.voice_watermark)
    }
}

impl VoiceActivationRecorder for CpalAudioRecorder {
    fn start(&mut self) -> Result<(), AudioCaptureError> {
        DictationRecorder::start(self)
    }

    fn cancel(&mut self) -> Result<(), AudioCaptureError> {
        DictationRecorder::cancel(self)
    }

    fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        let samples = self
            .buffer
            .as_ref()
            .ok_or_else(|| AudioCaptureError::new("audio capture buffer is unavailable"))?
            .drain()?;

        captured_audio_from_interleaved_input(self.sample_rate_hz, self.channels, &samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_recorder_keeps_level_callback_when_set_through_the_dictation_recorder() {
        let mut recorder = CpalAudioRecorder::new();
        let callback: AudioLevelCallback = std::sync::Arc::new(|_| {});

        DictationRecorder::set_level_callback(&mut recorder, Some(callback));

        assert!(
            recorder.level_callback.is_some(),
            "the trait dispatch the session uses must install the callback"
        );
    }

    #[test]
    fn sample_format_dispatch_covers_the_supported_formats_and_rejects_the_rest() {
        for format in [
            cpal::SampleFormat::I8,
            cpal::SampleFormat::I16,
            cpal::SampleFormat::I32,
            cpal::SampleFormat::U8,
            cpal::SampleFormat::U16,
            cpal::SampleFormat::U32,
            cpal::SampleFormat::F32,
            cpal::SampleFormat::F64,
        ] {
            assert!(
                stream_builder_for(format).is_some(),
                "{format} must have a dispatch row"
            );
        }

        for format in [
            cpal::SampleFormat::I24,
            cpal::SampleFormat::I64,
            cpal::SampleFormat::U24,
            cpal::SampleFormat::U64,
            cpal::SampleFormat::DsdU8,
        ] {
            assert!(
                stream_builder_for(format).is_none(),
                "{format} is unsupported"
            );
        }
    }
}
