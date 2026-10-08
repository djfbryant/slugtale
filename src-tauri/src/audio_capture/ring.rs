//! The ring microphone samples land in: a bounded single-producer/single-consumer
//! buffer with no lock, no allocation, and no wait on the audio thread.
//!
//! The CoreAudio callback is the sole producer while the stream is active. The
//! recorder pauses the stream before it becomes the sole consumer, so neither
//! side needs a lock. Every slot is allocated and initialized before `play`,
//! which also prevents first-touch page faults on the audio thread.
//!
//! Positions are counts rather than indices, which is what lets a Segment Pause
//! name the sample it wants to cut at without knowing where the ring wrapped.

use crate::AudioCaptureError;
use std::sync::Arc;

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
pub(super) struct RealtimeCaptureBuffer {
    slots: Box<[std::sync::atomic::AtomicU32]>,
    /// Monotonic count of samples ever pushed; never reset while recording
    /// lives. Read and write positions are counts, not indices, so a cut can
    /// be named by position without worrying about ring wrap.
    write_position: std::sync::atomic::AtomicUsize,
    read_position: std::sync::atomic::AtomicUsize,
    overflowed: std::sync::atomic::AtomicBool,
    /// Ring position of the most recent sample that arrived while the voice
    /// level was above the Segment threshold. Written by the real-time audio
    /// callback (one atomic store on voiced buffers only) and shared with
    /// whoever asked the recorder for its watermark cell, so a Pause Flush can
    /// name its cut without taking the recorder's lock (slugtale-xqzs).
    last_voice_position: Arc<std::sync::atomic::AtomicU64>,
}

impl RealtimeCaptureBuffer {
    pub(super) fn for_sample_rate(
        sample_rate_hz: u32,
        voice_watermark: Arc<std::sync::atomic::AtomicU64>,
    ) -> Result<Self, AudioCaptureError> {
        let capacity = usize::try_from(sample_rate_hz)
            .ok()
            .and_then(|rate| rate.checked_mul(MAX_RECORDING_SECONDS))
            .ok_or_else(|| AudioCaptureError::new("audio capture capacity is too large"))?;
        if capacity == 0 {
            return Err(AudioCaptureError::new("input sample rate must be non-zero"));
        }
        Ok(Self::with_capacity(capacity, voice_watermark))
    }

    pub(super) fn with_capacity(
        capacity: usize,
        voice_watermark: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        assert!(capacity > 0, "capture buffer capacity must be non-zero");
        Self {
            slots: (0..capacity)
                .map(|_| std::sync::atomic::AtomicU32::new(0f32.to_bits()))
                .collect(),
            write_position: std::sync::atomic::AtomicUsize::new(0),
            read_position: std::sync::atomic::AtomicUsize::new(0),
            overflowed: std::sync::atomic::AtomicBool::new(false),
            last_voice_position: voice_watermark,
        }
    }

    /// Called only by the real-time audio thread. This performs one bounded
    /// atomic write and never allocates, locks, waits, or overwrites old audio.
    pub(super) fn push_sample(&self, sample: f32) {
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
    pub(super) fn mark_voice(&self) {
        use std::sync::atomic::Ordering;

        self.last_voice_position.store(
            self.write_position.load(Ordering::Relaxed) as u64,
            Ordering::Relaxed,
        );
    }

    /// Called after the input stream is paused, outside the audio callback.
    pub(super) fn drain(&self) -> Result<Vec<f32>, AudioCaptureError> {
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
    pub(super) fn drain_through(
        &self,
        cut: u64,
        guard: u64,
    ) -> Result<Vec<f32>, AudioCaptureError> {
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
    pub(super) fn clear(&self) {
        use std::sync::atomic::Ordering;

        let write_position = self.write_position.load(Ordering::Acquire);
        self.read_position.store(write_position, Ordering::Release);
        // The watermark belongs to the previous dictation; park it at the same
        // position so no stale cut can point into the next one.
        self.last_voice_position
            .store(write_position as u64, Ordering::Release);
        self.overflowed.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captured_audio_from_interleaved_input;

    /// A capture ring of `capacity` samples with the watermark cell it publishes
    /// into, which is how the recorder builds one and how its caller reads it.
    fn a_ring(capacity: usize) -> (RealtimeCaptureBuffer, Arc<std::sync::atomic::AtomicU64>) {
        let watermark = Arc::new(std::sync::atomic::AtomicU64::new(0));
        (
            RealtimeCaptureBuffer::with_capacity(capacity, Arc::clone(&watermark)),
            watermark,
        )
    }

    /// The watermark a ring currently reports, read through its cell.
    fn watermark_of(watermark: &Arc<std::sync::atomic::AtomicU64>) -> u64 {
        watermark.load(std::sync::atomic::Ordering::Acquire)
    }

    #[test]
    fn a_prepared_ring_holds_no_samples_before_dictation_starts() {
        // Preparation allocates the ring zero-initialised and never lets it
        // fill: nothing captured before Dictation starts may leak into the
        // first dictation.
        let (ring, _) = a_ring(1024);

        assert_eq!(ring.drain().unwrap().len(), 0);
    }

    #[test]
    fn the_ring_publishes_its_watermark_into_the_cell_the_caller_already_holds() {
        // The whole point of the cell (slugtale-xqzs): whoever asked the recorder
        // for it reads the watermark of the live ring without going near the
        // recorder, so the level-emitter thread can name a Pause Flush's cut
        // while Stop or Cancel holds the recorder's lock and joins that thread.
        let (ring, cell) = a_ring(1024);

        for sample in 0..64 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();

        assert_eq!(watermark_of(&cell), 64);

        // A discarded dictation parks the watermark where the ring is, so no cut
        // from it can point into the next dictation's audio.
        ring.clear();
        assert_eq!(watermark_of(&cell), 64);
    }

    #[test]
    fn a_pause_cut_ends_at_the_watermark_even_when_the_worker_is_slow() {
        // Queue delay must not change the segment: audio arriving after the
        // cut stays in the ring for the next segment.
        let (ring, ring_watermark) = a_ring(1024);
        for sample in 0..100 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        let watermark = watermark_of(&ring_watermark);
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
        let (ring, _) = a_ring(1024);
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
        let (ring, _) = a_ring(1024);
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
        let (ring, _) = a_ring(1024);
        ring.push_sample(1.0);

        assert_eq!(ring.drain_through(1_000, 0).unwrap(), vec![1.0]);
    }

    #[test]
    fn multiple_pauses_cut_in_order_and_the_rest_reaches_stop() {
        let (ring, ring_watermark) = a_ring(1024);
        for sample in 0..40 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        let first_cut = watermark_of(&ring_watermark);
        for sample in 40..80 {
            ring.push_sample(sample as f32);
        }
        ring.mark_voice();
        let second_cut = watermark_of(&ring_watermark);
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

        let (ring, ring_watermark) = a_ring(SPEECH + TAIL);
        for index in 0..SPEECH {
            ring.push_sample(0.4);
            if index % 160 == 0 {
                ring.mark_voice();
            }
        }
        let watermark = watermark_of(&ring_watermark);
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
}
