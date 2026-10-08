//! The cpal backend: the audio stream, the ring it fills, and the level
//! publisher its callback feeds.
//!
//! This is the only module that names a device, and everything it does is in
//! service of one rule — the real-time audio callback must never allocate, lock,
//! wait, or cross an IPC boundary. A Tauri `emit` from inside it stalls the
//! ALSA/PipeWire period and the driver drops capture buffers, garbling the
//! transcription (slugtale-65l). The callback therefore stores one level in an
//! atomic and one sample per frame in the ring, and a separate emitter thread
//! forwards the level at a UI cadence.
//!
//! The rest is the preparation and reuse policy the recorder follows: build the
//! stream stopped, keep it across dictations while the device and format are
//! unchanged, and rebuild on any change.

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

/// Builds the input stream for one concrete sample type; one row of
/// [`INPUT_STREAM_BUILDERS`].
type StreamBuilder =
    fn(
        &cpal::Device,
        &cpal::StreamConfig,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
        Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<(cpal::Stream, std::sync::Arc<RealtimeCaptureBuffer>), AudioCaptureError>;

/// The input sample formats the capture callback can convert, each with its
/// concrete stream builder. The supported-format policy is this table: a
/// format without a row (cpal's 24-bit, 64-bit, and DSD encodings) fails
/// stream construction as unsupported.
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
    /// The voiced-sample watermark every ring this recorder builds publishes
    /// into. Created once, for the recorder's whole life, so a caller holding
    /// the cell keeps reading the current ring's watermark across a stream
    /// rebuild (slugtale-xqzs).
    voice_watermark: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// How far idle-time preparation has got; see [`PrepareState`].
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
                    // Real-time audio callback: the pre-allocated ring and
                    // atomics below perform no allocation, locking, waiting, or
                    // IPC. Input is downmixed before storage so the ring's
                    // capacity maps exactly to five minutes of dictation rather
                    // than multiplying memory by the device channel count.
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
                        // Pair the perceptual level with the ring position it
                        // belongs to: the watermark a Pause Flush cuts at.
                        // One extra atomic store on voiced buffers only — the
                        // callback stays lock-free and allocation-free.
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

    /// Hardware-bound: builds a real cpal stream for the device. The
    /// supported-format policy above is unit-tested; this wrapper is not.
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

    /// Build a stream for `identity` and hold it with its ring, replacing any
    /// previous stream. Never plays: the microphone stays off until `play`
    /// (see [`DictationRecorder::prepare`]). The old stream is dropped first so a
    /// failed build leaves nothing stale behind.
    fn install_stream(
        &mut self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        identity: &InputStreamIdentity,
    ) -> Result<(), AudioCaptureError> {
        use std::sync::atomic::Ordering;

        self.stream.take();
        self.buffer = None;
        // A new ring starts a new dictation's watermark: a cut left over from the
        // previous one would point into audio that is not there.
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
                // Dropping the stream still stops capture. Forget its identity so
                // the next start builds fresh rather than reusing the failed one.
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

    /// Validate the default input device, allocate the capture ring, and build
    /// the input stream stopped while the app is idle, so the first Hotkey only
    /// pays for `play`. Never plays the stream (that is what activates the
    /// microphone) and never requests permission — see [`DictationRecorder::prepare`].
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
            // The callback downmixes each input frame before placing it in the
            // ring, exactly as Start configures it.
            self.channels = 1;

            // Building allocates the ring zero-initialised too, so first-touch
            // page faults land here rather than on the hotkey path.
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

        // The chosen device is part of the stream identity below, so turning
        // the preference on or off rebuilds the stream rather than reusing one
        // opened on the other microphone.
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
        // The callback downmixes each input frame before placing it in the ring.
        self.channels = 1;
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.clear();
        }

        self.level_bits
            .store(0f32.to_bits(), std::sync::atomic::Ordering::Relaxed);

        // Building a CoreAudio stream costs hundreds of milliseconds and was
        // paid on every hotkey press. Keep the paused stream when the default
        // device and format are unchanged; `stop`/`cancel` pause it, and `play`
        // resumes it in roughly 40 ms on the reference Mac (slugtale-op3).
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

            // A retained stream can become unusable after a device interruption
            // without its identity changing. Fall back to a cold start once so a
            // stale stream never strands dictation.
            self.install_stream(&device, &config, &identity)?;
            self.stream
                .as_ref()
                .expect("rebuilt audio stream exists")
                .play()
                .map_err(|error| AudioCaptureError::new(error.to_string()))?;
        }
        self.stream_active = true;
        // Start has validated the device and format right now, which is the
        // freshest preparation there is: record it so a later `prepare` call
        // knows the work is already done.
        self.prepare_state = PrepareState::Prepared { identity };

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

/// The listener opens and closes the same stream; only taking a chunk differs,
/// so it borrows the Dictation implementation rather than copying the device
/// work behind it.
impl VoiceActivationRecorder for CpalAudioRecorder {
    fn start(&mut self) -> Result<(), AudioCaptureError> {
        DictationRecorder::start(self)
    }

    fn cancel(&mut self) -> Result<(), AudioCaptureError> {
        DictationRecorder::cancel(self)
    }

    fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        // Deliberately no pause and no emitter shutdown: the stream keeps
        // running and the ring keeps filling behind this read. That is safe
        // because `RealtimeCaptureBuffer` is a single-producer/single-consumer
        // ring whose Acquire/Release pairs already order the audio thread's
        // writes against this drain — the caller is simply becoming the consumer
        // earlier than `stop` would.
        //
        // The one cost is that the resampler's window cannot span the cut, so a
        // segment boundary loses sub-millisecond accuracy at the join. Boundaries
        // only ever fall in the middle of a five-second silence, so there is no
        // speech there to damage.
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

        // The 24-bit, 64-bit, and DSD encodings have no row: they fail stream
        // construction as unsupported rather than mistranscribing samples.
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
