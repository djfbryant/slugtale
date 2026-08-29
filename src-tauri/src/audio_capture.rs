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

pub const DIGITAL_SILENCE_EPSILON: f32 = 0.000_01;

fn require_captured_microphone_signal(audio: &CapturedAudio) -> Result<(), AudioCaptureError> {
    // A denied macOS microphone does not fail the CoreAudio stream. It supplies
    // a correctly timed buffer of digital silence instead, which Whisper
    // canonically transcribes as "You" (slugtale-d3k). Real microphones have a
    // noise floor above this -100 dBFS threshold even in a quiet room.
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
    /// This is the whole reason capture and transcription can now overlap. It
    /// must not drop a single sample: whatever arrives while the returned
    /// segment is decoding belongs to the next one.
    fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError>;

    /// Like [`AudioRecorder::take_segment`], but drain only through a stable
    /// sample watermark plus the module's documented quiet-tail guard
    /// ([`QUIET_TAIL_GUARD`]), leaving anything later in the ring for the next
    /// segment. A Pause Flush cuts at the last voiced sample it knows about,
    /// so queue delay cannot append later speech or extra silence to this one
    /// (slugtale-g1o.4).
    fn take_segment_through(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError>;

    /// The ring position of the most recent voiced sample — the watermark a
    /// queued Pause Flush should carry as its cut. `0` when nothing voiced has
    /// been captured since the ring was cleared.
    fn voice_watermark(&self) -> u64 {
        0
    }

    /// Install the real-time-safe level publisher (see [`AudioLevelCallback`]).
    /// Only backends with a live audio callback distribute levels; the default
    /// records nothing.
    fn set_level_callback(&mut self, _callback: Option<AudioLevelCallback>) {}
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

const MAX_RECORDING_SECONDS: usize = 5 * 60;
const RECORDING_LIMIT_ERROR: &str = "recording exceeded the five-minute capture limit";

/// How much quiet audio is kept *after* the last heard voice when a Segment
/// Pause cuts its segment short.
///
/// This is an acoustic guard, not dead weight: word-final consonants decay
/// below the perceptual voice threshold before they finish sounding, the level
/// the watermark watches arrives one emitter tick late (~33 ms), and ASR
/// benefits from a little trailing room. 250 ms covers all three with margin
/// while staying far below the 500 ms ceiling slugtale-g1o.4 allows — against
/// the five-second Segment Pause this removes roughly 95 percent of the quiet
/// tail a segment used to carry to Transcription.
pub const QUIET_TAIL_GUARD: std::time::Duration = std::time::Duration::from_millis(250);

/// A bounded single-producer/single-consumer ring for microphone samples.
///
/// The CoreAudio callback is the sole producer while the stream is active. The
/// recorder pauses the stream before it becomes the sole consumer, so neither
/// side needs a lock. Every slot is allocated and initialized before `play`,
/// which also prevents first-touch page faults on the audio thread.
struct RealtimeCaptureBuffer {
    slots: Box<[std::sync::atomic::AtomicU32]>,
    /// Monotonic count of samples ever pushed; never reset while recording
    /// lives. Read and write positions are counts, not indices, so a cut can
    /// be named by position without worrying about ring wrap.
    write_position: std::sync::atomic::AtomicUsize,
    read_position: std::sync::atomic::AtomicUsize,
    overflowed: std::sync::atomic::AtomicBool,
    /// Ring position of the most recent sample that arrived while the voice
    /// level was above the Segment threshold. Written by the real-time audio
    /// callback (one atomic store on voiced buffers only), read when a Pause
    /// Flush names its cut (slugtale-g1o.4).
    last_voice_position: std::sync::atomic::AtomicUsize,
}

impl RealtimeCaptureBuffer {
    fn for_sample_rate(sample_rate_hz: u32) -> Result<Self, AudioCaptureError> {
        let capacity = usize::try_from(sample_rate_hz)
            .ok()
            .and_then(|rate| rate.checked_mul(MAX_RECORDING_SECONDS))
            .ok_or_else(|| AudioCaptureError::new("audio capture capacity is too large"))?;
        if capacity == 0 {
            return Err(AudioCaptureError::new("input sample rate must be non-zero"));
        }
        Ok(Self::with_capacity(capacity))
    }

    fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "capture buffer capacity must be non-zero");
        Self {
            slots: (0..capacity)
                .map(|_| std::sync::atomic::AtomicU32::new(0f32.to_bits()))
                .collect(),
            write_position: std::sync::atomic::AtomicUsize::new(0),
            read_position: std::sync::atomic::AtomicUsize::new(0),
            overflowed: std::sync::atomic::AtomicBool::new(false),
            last_voice_position: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Called only by the real-time audio thread. This performs one bounded
    /// atomic write and never allocates, locks, waits, or overwrites old audio.
    fn push_sample(&self, sample: f32) {
        use std::sync::atomic::Ordering;

        let write_position = self.write_position.load(Ordering::Relaxed);
        let read_position = self.read_position.load(Ordering::Acquire);
        if write_position.wrapping_sub(read_position) >= self.slots.len() {
            self.overflowed.store(true, Ordering::Relaxed);
            return;
        }

        let slot = write_position % self.slots.len();
        self.slots[slot].store(sample.to_bits(), Ordering::Relaxed);
        self.write_position
            .store(write_position.wrapping_add(1), Ordering::Release);
    }

    /// Record, from the real-time callback, that the sample just written was
    /// voiced. One relaxed atomic store on voiced buffers only: lock-free,
    /// allocation-free, and skipped entirely on quiet buffers.
    fn mark_voice(&self) {
        use std::sync::atomic::Ordering;

        self.last_voice_position.store(
            self.write_position.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    /// The ring position of the most recent voiced sample, as a stable
    /// monotonic watermark a queued Pause Flush can cut at.
    pub fn voice_watermark(&self) -> u64 {
        self.last_voice_position
            .load(std::sync::atomic::Ordering::Acquire) as u64
    }

    /// Called after the input stream is paused, outside the audio callback.
    fn drain(&self) -> Result<Vec<f32>, AudioCaptureError> {
        let read_position = self
            .read_position
            .load(std::sync::atomic::Ordering::Relaxed);
        let write_position = self
            .write_position
            .load(std::sync::atomic::Ordering::Acquire);
        self.read_range(read_position, write_position)
    }

    /// Drain only through `cut` plus a small acoustic guard, leaving anything
    /// after it in the ring for the next segment (slugtale-g1o.4).
    ///
    /// `cut` is a stable watermark — the ring position of the last voiced
    /// sample when a Pause Flush was queued — so worker queue delay cannot add
    /// later speech or silence to this segment. Wrap and overrun behaviour is
    /// defined here:
    ///
    /// - A cut already handed over (at or behind the read position) yields
    ///   nothing; the guard never rewinds into drained audio.
    /// - A cut ahead of production (only possible for a stale job) drains
    ///   through whatever exists rather than blocking.
    /// - The producer having lapped the consumer is an overflow, exactly as in
    ///   [`Self::drain`].
    fn drain_through(&self, cut: u64, guard: u64) -> Result<Vec<f32>, AudioCaptureError> {
        let read_position = self
            .read_position
            .load(std::sync::atomic::Ordering::Relaxed);
        let write_position = self
            .write_position
            .load(std::sync::atomic::Ordering::Acquire);

        let cut = cut.min(usize::MAX as u64) as usize;
        // Never read later samples than exist, and never rewind into audio a
        // previous segment already took.
        let end = write_position.min(cut.saturating_add(guard as usize));
        if end <= read_position {
            return Ok(Vec::new());
        }
        self.read_range(read_position, end)
    }

    /// Read `[from, to)` and advance the read position to `to`. Shared by
    /// [`Self::drain`] and [`Self::drain_through`]; both callers have already
    /// clamped their range.
    fn read_range(&self, from: usize, to: usize) -> Result<Vec<f32>, AudioCaptureError> {
        use std::sync::atomic::Ordering;

        let available = to.wrapping_sub(from).min(self.slots.len());
        let mut samples = Vec::with_capacity(available);
        for offset in 0..available {
            let slot = from.wrapping_add(offset) % self.slots.len();
            samples.push(f32::from_bits(self.slots[slot].load(Ordering::Relaxed)));
        }
        self.read_position.store(to, Ordering::Release);

        if self.overflowed.swap(false, Ordering::Relaxed) {
            return Err(AudioCaptureError::new(RECORDING_LIMIT_ERROR));
        }
        Ok(samples)
    }

    /// Discard pending audio between dictations without reallocating the ring.
    fn clear(&self) {
        use std::sync::atomic::Ordering;

        let write_position = self.write_position.load(Ordering::Acquire);
        self.read_position.store(write_position, Ordering::Release);
        // The watermark belongs to the previous dictation; park it at the same
        // position so no stale cut can point into the next one.
        self.last_voice_position
            .store(write_position, Ordering::Release);
        self.overflowed.store(false, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InputStreamIdentity {
    device_id: Option<cpal::DeviceId>,
    sample_format: cpal::SampleFormat,
    sample_rate_hz: u32,
    channels: u16,
}

/// Whether the paused stream from the previous dictation may serve the next
/// one. Reuse demands every fact about the stream to be unchanged: the
/// recorder must still hold the stream and its ring, and the observed
/// device/format identity must equal the recorded one. Anything less takes
/// the cold-start path and rebuilds all three together (slugtale-op3).
fn paused_stream_is_reusable(
    stream_held: bool,
    buffer_held: bool,
    recorded: Option<&InputStreamIdentity>,
    observed: &InputStreamIdentity,
) -> bool {
    stream_held && buffer_held && recorded == Some(observed)
}

/// Where the recorder stands relative to the first Hotkey. `Recording` is not
/// a variant here because it is already tracked by `stream_active`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum PrepareState {
    #[default]
    Unprepared,
    /// Device and format validated and the stream built in a stopped state;
    /// Start only has to play it. See [`AudioRecorder::prepare`] for why a
    /// stopped stream is safe to hold.
    Prepared { identity: InputStreamIdentity },
    /// The last prepare attempt failed with this message. A later prepare may
    /// retry — a missing device can come back.
    Failed(String),
}

/// Builds the input stream for one concrete sample type; one row of
/// [`INPUT_STREAM_BUILDERS`].
type StreamBuilder = fn(
    &cpal::Device,
    &cpal::StreamConfig,
    std::sync::Arc<std::sync::atomic::AtomicU32>,
) -> Result<(cpal::Stream, std::sync::Arc<RealtimeCaptureBuffer>), AudioCaptureError>;

/// The input sample formats the capture callback can convert, each with its
/// concrete stream builder. The supported-format policy is this table: a
/// format without a row (cpal's 24-bit, 64-bit, and DSD encodings) fails
/// stream construction as unsupported.
const INPUT_STREAM_BUILDERS: &[(cpal::SampleFormat, StreamBuilder)] = &[
    (cpal::SampleFormat::I8, CpalAudioRecorder::build_stream::<i8>),
    (cpal::SampleFormat::I16, CpalAudioRecorder::build_stream::<i16>),
    (cpal::SampleFormat::I32, CpalAudioRecorder::build_stream::<i32>),
    (cpal::SampleFormat::U8, CpalAudioRecorder::build_stream::<u8>),
    (cpal::SampleFormat::U16, CpalAudioRecorder::build_stream::<u16>),
    (cpal::SampleFormat::U32, CpalAudioRecorder::build_stream::<u32>),
    (cpal::SampleFormat::F32, CpalAudioRecorder::build_stream::<f32>),
    (cpal::SampleFormat::F64, CpalAudioRecorder::build_stream::<f64>),
];

fn stream_builder_for(sample_format: cpal::SampleFormat) -> Option<StreamBuilder> {
    INPUT_STREAM_BUILDERS
        .iter()
        .find(|(format, _)| *format == sample_format)
        .map(|(_, builder)| *builder)
}

/// Whether an idle-time prepare should run at all. A prepared state has
/// nothing left to do, and while a stream is held (recording or paused)
/// preparation must not disturb it; a failure may always be retried — a
/// missing device can come back.
fn should_attempt_prepare(state: &PrepareState, stream_held: bool) -> bool {
    !matches!(state, PrepareState::Prepared { .. }) && !stream_held
}

/// Fold a prepare attempt into the next idle-preparation state: success
/// records the device/format identity, failure records why. The recorder
/// replaces its whole state with this result each attempt, so a later
/// success overwrites an earlier failure.
fn prepare_state_after(
    outcome: Result<&InputStreamIdentity, &AudioCaptureError>,
) -> PrepareState {
    match outcome {
        Ok(identity) => PrepareState::Prepared {
            identity: identity.clone(),
        },
        Err(error) => PrepareState::Failed(error.to_string()),
    }
}

#[derive(Default)]
pub struct CpalAudioRecorder {
    stream: Option<cpal::Stream>,
    stream_identity: Option<InputStreamIdentity>,
    stream_active: bool,
    buffer: Option<std::sync::Arc<RealtimeCaptureBuffer>>,
    /// How far idle-time preparation has got; see [`PrepareState`].
    prepare_state: PrepareState,
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
        level_bits: std::sync::Arc<std::sync::atomic::AtomicU32>,
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
        let buffer =
            std::sync::Arc::new(RealtimeCaptureBuffer::for_sample_rate(config.sample_rate)?);
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
                        if level > crate::SEGMENT_VOICE_LEVEL {
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
    ) -> Result<(cpal::Stream, std::sync::Arc<RealtimeCaptureBuffer>), AudioCaptureError> {
        let builder = stream_builder_for(sample_format).ok_or_else(|| {
            AudioCaptureError::new(format!("unsupported input sample format: {sample_format}"))
        })?;
        builder(device, config, level_bits)
    }

    /// Build a stream for `identity` and hold it with its ring, replacing any
    /// previous stream. Never plays: the microphone stays off until `play`
    /// (see [`AudioRecorder::prepare`]). The old stream is dropped first so a
    /// failed build leaves nothing stale behind.
    fn install_stream(
        &mut self,
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        identity: &InputStreamIdentity,
    ) -> Result<(), AudioCaptureError> {
        self.stream.take();
        self.buffer = None;
        let (stream, buffer) = Self::build_stream_for_format(
            device,
            config,
            identity.sample_format,
            self.level_bits.clone(),
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

impl AudioRecorder for CpalAudioRecorder {
    fn set_level_callback(&mut self, callback: Option<AudioLevelCallback>) {
        CpalAudioRecorder::set_level_callback(self, callback);
    }

    /// Validate the default input device, allocate the capture ring, and build
    /// the input stream stopped while the app is idle, so the first Hotkey only
    /// pays for `play`. Never plays the stream (that is what activates the
    /// microphone) and never requests permission — see [`AudioRecorder::prepare`].
    fn prepare(&mut self) -> Result<(), AudioCaptureError> {
        use cpal::traits::{DeviceTrait, HostTrait};

        if !should_attempt_prepare(&self.prepare_state, self.stream.is_some()) {
            return Ok(());
        }

        let prepared = (|| -> Result<InputStreamIdentity, AudioCaptureError> {
            let host = cpal::default_host();
            let device = host
                .default_input_device()
                .ok_or_else(|| AudioCaptureError::new("no default input device is available"))?;
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
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        self.pause_active_stream();
        self.stop_level_emitter();

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioCaptureError::new("no default input device is available"))?;
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

    fn take_segment_through(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
        let buffer = self
            .buffer
            .as_ref()
            .ok_or_else(|| AudioCaptureError::new("audio capture buffer is unavailable"))?;
        let guard = (QUIET_TAIL_GUARD.as_secs_f64() * f64::from(self.sample_rate_hz)) as u64;
        let samples = buffer.drain_through(cut, guard)?;

        captured_audio_from_interleaved_input(self.sample_rate_hz, self.channels, &samples)
    }

    fn voice_watermark(&self) -> u64 {
        self.buffer
            .as_ref()
            .map(|buffer| buffer.voice_watermark())
            .unwrap_or(0)
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
    /// Whether this dictation has already handed a Dictation Segment off to be
    /// transcribed. It decides whether the digital-silence guard still applies
    /// when the dictation ends.
    flushed_a_segment: bool,
}

impl<R> AudioCaptureSession<R>
where
    R: AudioRecorder,
{
    pub fn new(recorder: R) -> Self {
        Self {
            recorder,
            active: false,
            flushed_a_segment: false,
        }
    }

    /// Take the speech captured so far as a Dictation Segment, leaving the
    /// recording running. Returns `None` when there is nothing to take — either
    /// no dictation is active, or the ring has been drained since the last
    /// Segment Pause.
    pub fn flush_segment(&mut self) -> Result<Option<CapturedAudio>, AudioCaptureError> {
        if !self.active {
            return Ok(None);
        }

        let audio = self.recorder.take_segment()?;
        if audio.samples.is_empty() {
            return Ok(None);
        }

        self.flushed_a_segment = true;
        Ok(Some(audio))
    }

    /// Like [`Self::flush_segment`], but cut at a stable sample watermark: only
    /// audio through `cut` plus the quiet-tail guard joins this segment, so a
    /// slow worker queue cannot append later speech to it (slugtale-g1o.4).
    pub fn flush_segment_through(
        &mut self,
        cut: u64,
    ) -> Result<Option<CapturedAudio>, AudioCaptureError> {
        if !self.active {
            return Ok(None);
        }

        let audio = self.recorder.take_segment_through(cut)?;
        if audio.samples.is_empty() {
            return Ok(None);
        }

        self.flushed_a_segment = true;
        Ok(Some(audio))
    }

    pub fn on_event(
        &mut self,
        event: DictationEvent,
    ) -> Result<Option<AudioCaptureOutcome>, AudioCaptureError> {
        match event {
            DictationEvent::Start => {
                self.recorder.start()?;
                self.active = true;
                self.flushed_a_segment = false;
                Ok(None)
            }
            DictationEvent::Stop if self.active => {
                self.active = false;
                let audio = self.recorder.stop()?;
                // The digital-silence guard catches a denied microphone, which
                // supplies perfectly timed silence rather than failing. It can
                // only speak for a dictation that flushed nothing: once a
                // Segment Pause has handed real speech off, ending on silence is
                // exactly what a user who paused and then pressed Stop produces.
                // A denied microphone never reaches that state, because a level
                // pinned at zero never opens a Segment Pause in the first place.
                if !self.flushed_a_segment {
                    require_captured_microphone_signal(&audio)?;
                }
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

/// Always-on microphone used by Voice Activation.
///
/// Dictation's [`CpalAudioRecorder`] keeps a paused CoreAudio stream so the next
/// hotkey only pays for `play`. The listener cannot share that trick: after
/// dictation or digital silence, `play` on the retained stream can succeed while
/// the callback supplies only zeros (slugtale-3wo). Closing therefore drops the
/// recorder and the next start is given a fresh one.
pub struct VoiceActivationCapture<R: AudioRecorder> {
    recorder: R,
    open: bool,
}

impl<R: AudioRecorder> VoiceActivationCapture<R> {
    pub fn new(recorder: R) -> Self {
        Self {
            recorder,
            open: false,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn start(&mut self) -> Result<(), AudioCaptureError> {
        if self.open {
            return Ok(());
        }
        self.recorder.start()?;
        self.open = true;
        Ok(())
    }

    pub fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
        self.recorder.take_segment()
    }

    /// Stop capture if it is running, then replace the recorder so the next
    /// start cannot resume a paused stream.
    pub fn rebuild(&mut self, next: R) {
        self.close();
        self.recorder = next;
    }

    pub fn close(&mut self) {
        if !self.open {
            return;
        }
        let _ = self.recorder.cancel();
        self.open = false;
    }
}

impl<R: AudioRecorder> Drop for VoiceActivationCapture<R> {
    fn drop(&mut self) {
        self.close();
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
    fn cpal_recorder_keeps_level_callback_when_set_through_audio_recorder() {
        let mut recorder = CpalAudioRecorder::new();
        let callback: AudioLevelCallback = std::sync::Arc::new(|_| {});

        AudioRecorder::set_level_callback(&mut recorder, Some(callback));

        assert!(
            recorder.level_callback.is_some(),
            "the trait dispatch used by DictationHost must install the callback"
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

    #[test]
    fn idle_prepare_runs_only_when_failed_or_unprepared_and_no_stream_is_held() {
        let prepared = PrepareState::Prepared {
            identity: InputStreamIdentity {
                device_id: None,
                sample_format: cpal::SampleFormat::F32,
                sample_rate_hz: 48_000,
                channels: 1,
            },
        };

        assert!(should_attempt_prepare(&PrepareState::Unprepared, false));
        assert!(!should_attempt_prepare(&prepared, false));
        // Preparing while recording must not disturb the dictation in
        // progress.
        assert!(!should_attempt_prepare(&PrepareState::Unprepared, true));
        // A failed prepare may always be retried: a missing device can come
        // back.
        assert!(should_attempt_prepare(
            &PrepareState::Failed("no device".into()),
            false
        ));
    }

    #[test]
    fn a_prepare_outcome_is_recorded_as_the_identity_or_the_failure_reason() {
        let identity = InputStreamIdentity {
            device_id: None,
            sample_format: cpal::SampleFormat::F32,
            sample_rate_hz: 48_000,
            channels: 1,
        };
        let error = AudioCaptureError::new("no default input device is available");

        assert_eq!(
            prepare_state_after(Ok(&identity)),
            PrepareState::Prepared {
                identity: identity.clone()
            }
        );
        assert_eq!(
            prepare_state_after(Err(&error)),
            PrepareState::Failed(error.to_string())
        );
    }

    #[test]
    fn a_paused_stream_is_reused_only_when_everything_about_it_still_matches() {
        let observed = InputStreamIdentity {
            device_id: None,
            sample_format: cpal::SampleFormat::F32,
            sample_rate_hz: 48_000,
            channels: 1,
        };
        let recorded = observed.clone();
        let changed = InputStreamIdentity {
            sample_rate_hz: 44_100,
            ..observed.clone()
        };

        // Same device and format: the hundreds-of-milliseconds rebuild is
        // skipped and `play` resumes the paused stream (slugtale-op3).
        assert!(paused_stream_is_reusable(
            true,
            true,
            Some(&recorded),
            &observed
        ));
        assert!(!paused_stream_is_reusable(
            true,
            true,
            Some(&changed),
            &observed
        ));

        // A dropped stream, a dropped ring, or a forgotten identity forces the
        // cold start that rebuilds all three together.
        assert!(!paused_stream_is_reusable(
            false,
            true,
            Some(&recorded),
            &observed
        ));
        assert!(!paused_stream_is_reusable(
            true,
            false,
            Some(&recorded),
            &observed
        ));
        assert!(!paused_stream_is_reusable(true, true, None, &observed));
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
    fn flushing_a_segment_keeps_the_recording_running_for_the_next_one() {
        let recorder = FakeAudioRecorder::flushing(
            CapturedAudio::mono_16khz(vec![0.3, 0.3]),
            vec![
                CapturedAudio::mono_16khz(vec![0.1, 0.1]),
                CapturedAudio::mono_16khz(vec![0.2, 0.2]),
            ],
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();

        let first = session.flush_segment().unwrap();
        let second = session.flush_segment().unwrap();
        let remainder = session.on_event(DictationEvent::Stop).unwrap();

        // Each segment is handed over exactly once, in the order it was spoken,
        // and Stop still returns whatever was captured after the last pause.
        assert_eq!(first, Some(CapturedAudio::mono_16khz(vec![0.1, 0.1])));
        assert_eq!(second, Some(CapturedAudio::mono_16khz(vec![0.2, 0.2])));
        assert_eq!(
            remainder,
            Some(AudioCaptureOutcome::Completed(CapturedAudio::mono_16khz(
                vec![0.3, 0.3]
            )))
        );
        assert_eq!(
            session.recorder().events.borrow().as_slice(),
            &["start", "take_segment", "take_segment", "stop"]
        );
    }

    #[test]
    fn flushing_a_drained_ring_yields_no_segment() {
        // Nothing new since the last Segment Pause must not enqueue an empty
        // segment for the transcription engine to chew on.
        let recorder = FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.2]));
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();

        assert_eq!(session.flush_segment().unwrap(), None);
    }

    #[test]
    fn flushing_outside_a_dictation_yields_no_segment() {
        let recorder = FakeAudioRecorder::flushing(
            CapturedAudio::mono_16khz(vec![0.2]),
            vec![CapturedAudio::mono_16khz(vec![0.1])],
        );
        let mut session = AudioCaptureSession::new(recorder);

        assert_eq!(session.flush_segment().unwrap(), None);
        assert!(session.recorder().events.borrow().is_empty());
    }

    #[test]
    fn preparing_twice_prepares_once_and_start_does_not_reprepare() {
        // Idle-time preparation is idempotent, and Start consumes the prepared
        // state rather than repeating the work on the Hotkey path.
        let mut recorder = FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.2]));
        recorder.prepare().unwrap();
        recorder.prepare().unwrap();
        assert_eq!(recorder.prepare_count(), 1);

        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();

        assert_eq!(session.recorder().prepare_count(), 1);
    }

    #[test]
    fn a_failed_prepare_does_not_block_the_next_dictation_start() {
        // Preparation is opportunistic: if the device cannot be validated while
        // idle — or comes back with an error — the Hotkey path must still work.
        let recorder =
            FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.2])).failing_prepare();
        let mut session = AudioCaptureSession::new(recorder);

        assert!(session.recorder_mut().prepare().is_err());

        assert_eq!(session.on_event(DictationEvent::Start).unwrap(), None);
        let completed = session.on_event(DictationEvent::Stop).unwrap();
        assert!(matches!(completed, Some(AudioCaptureOutcome::Completed(_))));
    }

    #[test]
    fn preparing_a_recording_recorder_changes_nothing() {
        // A prepare racing an active dictation (the caller holds the same mutex
        // the Hotkey uses) must not disturb the recording in progress.
        let recorder = FakeAudioRecorder::new(CapturedAudio::mono_16khz(vec![0.2]));
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();

        session.recorder_mut().prepare().unwrap();

        let completed = session.on_event(DictationEvent::Stop).unwrap();
        assert!(matches!(completed, Some(AudioCaptureOutcome::Completed(_))));
        assert_eq!(
            session.recorder().events.borrow().as_slice(),
            &["start", "stop"]
        );
    }

    #[test]
    fn a_prepared_ring_holds_no_samples_before_dictation_starts() {
        // Preparation allocates the ring zero-initialised and never lets it
        // fill: nothing captured before Dictation starts may leak into the
        // first dictation.
        let ring = RealtimeCaptureBuffer::with_capacity(1024);

        assert_eq!(ring.drain().unwrap().len(), 0);
    }

    #[test]
    fn a_dictation_that_already_flushed_speech_may_end_on_silence() {
        // The user paused, the pause was flushed and inserted, and then they
        // pressed Stop without speaking again. The remainder is genuinely silent
        // and must not be reported as a missing microphone.
        let recorder = FakeAudioRecorder::flushing(
            CapturedAudio::mono_16khz(vec![0.0; 16_000]),
            vec![CapturedAudio::mono_16khz(vec![0.4, -0.4])],
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();
        session.flush_segment().unwrap();

        let completed = session.on_event(DictationEvent::Stop).unwrap();

        assert!(matches!(completed, Some(AudioCaptureOutcome::Completed(_))));
    }

    #[test]
    fn a_new_dictation_restores_the_digital_silence_guard() {
        // The relaxation above must not leak into the next dictation, or a
        // microphone revoked between dictations would go unreported.
        let recorder = FakeAudioRecorder::flushing(
            CapturedAudio::mono_16khz(vec![0.0; 16_000]),
            vec![CapturedAudio::mono_16khz(vec![0.4, -0.4])],
        );
        let mut session = AudioCaptureSession::new(recorder);
        session.on_event(DictationEvent::Start).unwrap();
        session.flush_segment().unwrap();
        session.on_event(DictationEvent::Stop).unwrap();

        session.on_event(DictationEvent::Start).unwrap();
        let error = session.on_event(DictationEvent::Stop).unwrap_err();

        assert_eq!(
            error,
            AudioCaptureError::new(
                "no microphone signal was captured; check Slugtale under System Settings > Privacy & Security > Microphone"
            )
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

    #[test]
    fn a_pause_cut_ends_at_the_watermark_even_when_the_worker_is_slow() {
        // Queue delay must not change the segment: audio arriving after the
        // cut stays in the ring for the next segment.
        let ring = RealtimeCaptureBuffer::with_capacity(1024);
        for sample in 0..100 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        let watermark = ring.voice_watermark();
        for sample in 100..300 {
            // Speech continues while the flush sits in the queue.
            ring.push_sample(sample as f32);
            if sample < 120 {
                ring.mark_voice();
            }
        }

        let segment = ring.drain_through(watermark, 0).unwrap();

        assert_eq!(segment.len(), 100);
        assert_eq!(segment[99], 99.0);
        // Nothing is lost: the rest drains afterwards.
        let remainder = ring.drain().unwrap();
        assert_eq!(remainder.len(), 200);
    }

    #[test]
    fn the_quiet_tail_guard_keeps_a_documented_sliver_after_the_cut() {
        let ring = RealtimeCaptureBuffer::with_capacity(1024);
        for sample in 0..100 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        for _ in 100..130 {
            ring.push_sample(0.0);
        }

        let segment = ring.drain_through(100, 20).unwrap();

        assert_eq!(segment.len(), 120);
    }

    #[test]
    fn a_stale_cut_yields_nothing_and_never_rewinds() {
        let ring = RealtimeCaptureBuffer::with_capacity(1024);
        for sample in 0..50 {
            ring.push_sample(sample as f32);
        }
        assert_eq!(ring.drain().unwrap().len(), 50);

        // A duplicate or stale job pointing behind the read position takes
        // nothing and leaves the ring consistent.
        assert_eq!(ring.drain_through(10, 5).unwrap().len(), 0);

        for sample in 50..60 {
            ring.push_sample(sample as f32);
        }
        assert_eq!(ring.drain().unwrap().len(), 10);
    }

    #[test]
    fn a_cut_ahead_of_production_drains_what_exists_rather_than_blocking() {
        let ring = RealtimeCaptureBuffer::with_capacity(1024);
        ring.push_sample(1.0);

        assert_eq!(ring.drain_through(1_000, 0).unwrap(), vec![1.0]);
    }

    #[test]
    fn multiple_pauses_cut_in_order_and_the_rest_reaches_stop() {
        let ring = RealtimeCaptureBuffer::with_capacity(1024);
        for sample in 0..40 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        let first_cut = ring.voice_watermark();
        for sample in 40..80 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        let second_cut = ring.voice_watermark();
        for sample in 80..100 {
            ring.push_sample(sample as f32);
        }

        let first = ring.drain_through(first_cut, 0).unwrap();
        let second = ring.drain_through(second_cut, 0).unwrap();
        let remainder = ring.drain().unwrap();

        assert_eq!(first.len(), 40);
        assert_eq!(second.len(), 40);
        assert_eq!(remainder.len(), 20);
    }

    #[test]
    fn cutting_at_the_watermark_keeps_speech_intact_while_dropping_most_of_the_quiet_tail() {
        // The slugtale-g1o.4 win, measured on a synthetic phrase: half a second
        // of speech followed by the full five-second Segment Pause of silence.
        const RATE: usize = 16_000;
        const SPEECH: usize = RATE / 2;
        const TAIL: usize = RATE * 9 / 2;

        let ring = RealtimeCaptureBuffer::with_capacity(SPEECH + TAIL);
        for index in 0..SPEECH {
            ring.push_sample(0.4);
            if index % 160 == 0 {
                ring.mark_voice();
            }
        }
        let watermark = ring.voice_watermark();
        for _ in 0..TAIL {
            ring.push_sample(0.0);
        }

        let guard = (QUIET_TAIL_GUARD.as_secs_f64() * RATE as f64) as u64;
        let segment = ring
            .drain_through(watermark.min(SPEECH as u64), guard)
            .unwrap();
        let audio = captured_audio_from_interleaved_input(RATE as u32, 1, &segment).unwrap();

        // Correctness: every voiced sample survives into the segment handed to
        // Transcription.
        assert_eq!(
            segment.iter().filter(|sample| **sample != 0.0).count(),
            SPEECH
        );
        // And the transcript-critical signal shape is unchanged by the cut.
        assert!(!audio.samples.is_empty());

        // Efficiency: what used to be five seconds of audio is now speech plus
        // the guard — at least an 80 percent reduction.
        let total = (SPEECH + TAIL) as f64;
        let reduction = 1.0 - segment.len() as f64 / total;
        assert!(
            reduction >= 0.80,
            "expected at least 80 percent fewer samples, got {recession:.2}",
            recession = reduction
        );
    }

    struct FakeAudioRecorder {
        audio: CapturedAudio,
        /// What each successive `take_segment` hands back, mimicking a ring that
        /// is drained mid-recording and refills from the microphone.
        segments: std::cell::RefCell<std::collections::VecDeque<CapturedAudio>>,
        events: std::cell::RefCell<Vec<&'static str>>,
        prepares: std::cell::Cell<usize>,
        /// Mimics the real recorder's idempotence rule: preparation is skipped
        /// once prepared or while recording.
        prepared_or_recording: std::cell::Cell<bool>,
        fail_prepare: bool,
        watermark: std::cell::Cell<u64>,
    }

    impl FakeAudioRecorder {
        fn new(audio: CapturedAudio) -> Self {
            Self {
                audio,
                segments: std::cell::RefCell::new(std::collections::VecDeque::new()),
                events: std::cell::RefCell::new(Vec::new()),
                prepares: std::cell::Cell::new(0),
                prepared_or_recording: std::cell::Cell::new(false),
                fail_prepare: false,
                watermark: std::cell::Cell::new(0),
            }
        }

        fn flushing(audio: CapturedAudio, segments: Vec<CapturedAudio>) -> Self {
            let recorder = Self::new(audio);
            *recorder.segments.borrow_mut() = segments.into();
            recorder
        }

        fn failing_prepare(mut self) -> Self {
            self.fail_prepare = true;
            self
        }

        fn prepare_count(&self) -> usize {
            self.prepares.get()
        }
    }

    impl AudioRecorder for FakeAudioRecorder {
        fn prepare(&mut self) -> Result<(), AudioCaptureError> {
            if self.fail_prepare {
                return Err(AudioCaptureError::new("fake prepare failure"));
            }
            if !self.prepared_or_recording.replace(true) {
                self.prepares.set(self.prepares.get() + 1);
                self.events.borrow_mut().push("prepare");
            }
            Ok(())
        }

        fn start(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push("start");
            self.prepared_or_recording.set(true);
            Ok(())
        }

        fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            self.events.borrow_mut().push("stop");
            self.prepared_or_recording.set(false);
            Ok(self.audio.clone())
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push("cancel");
            self.prepared_or_recording.set(false);
            Ok(())
        }
        fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            self.events.borrow_mut().push("take_segment");
            Ok(self
                .segments
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| CapturedAudio::mono_16khz(Vec::new())))
        }

        fn take_segment_through(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
            self.events.borrow_mut().push("take_segment_through");
            let _ = cut;
            Ok(self
                .segments
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| CapturedAudio::mono_16khz(Vec::new())))
        }

        fn voice_watermark(&self) -> u64 {
            self.watermark.get()
        }
    }

    struct GenerationRecorder {
        generation: u32,
        events: std::rc::Rc<std::cell::RefCell<Vec<(u32, &'static str)>>>,
    }

    impl GenerationRecorder {
        fn new(
            generation: u32,
            events: std::rc::Rc<std::cell::RefCell<Vec<(u32, &'static str)>>>,
        ) -> Self {
            Self { generation, events }
        }
    }

    impl AudioRecorder for GenerationRecorder {
        fn prepare(&mut self) -> Result<(), AudioCaptureError> {
            Ok(())
        }

        fn start(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push((self.generation, "start"));
            Ok(())
        }

        fn stop(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            Ok(CapturedAudio::mono_16khz(Vec::new()))
        }

        fn cancel(&mut self) -> Result<(), AudioCaptureError> {
            self.events.borrow_mut().push((self.generation, "cancel"));
            Ok(())
        }

        fn take_segment(&mut self) -> Result<CapturedAudio, AudioCaptureError> {
            Ok(CapturedAudio::mono_16khz(vec![0.1]))
        }

        fn take_segment_through(&mut self, cut: u64) -> Result<CapturedAudio, AudioCaptureError> {
            let _ = cut;
            self.take_segment()
        }
    }

    #[test]
    fn a_second_listen_after_dictation_rebuilds_the_recorder() {
        // The installed-app failure: first "Hi Slugtale" starts dictation, then
        // later phrases do nothing because start() resumed the paused listener
        // stream and CoreAudio fed it digital silence. Rebuilding is the fix.
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut capture = VoiceActivationCapture::new(GenerationRecorder::new(1, events.clone()));

        capture.start().unwrap();
        capture.rebuild(GenerationRecorder::new(2, events.clone()));
        capture.start().unwrap();

        assert_eq!(
            events.borrow().as_slice(),
            &[(1, "start"), (1, "cancel"), (2, "start")]
        );
        assert!(capture.is_open());
    }

    #[test]
    fn the_listener_keeps_its_recorder_while_it_is_still_listening() {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut capture = VoiceActivationCapture::new(GenerationRecorder::new(1, events.clone()));

        capture.start().unwrap();
        capture.start().unwrap();
        let _ = capture.take_segment().unwrap();

        assert_eq!(events.borrow().as_slice(), &[(1, "start")]);
    }

    #[test]
    fn upsampling_doubles_sub_16khz_mono_input() {
        let audio =
            captured_audio_from_interleaved_input(8_000, 1, &[0.0, 1.0, 0.0, 1.0]).unwrap();

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

    #[test]
    fn voice_activation_capture_reports_open_while_listening() {
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut capture = VoiceActivationCapture::new(GenerationRecorder::new(1, events));

        assert!(!capture.is_open());
        capture.start().unwrap();
        assert!(capture.is_open());
    }
}
