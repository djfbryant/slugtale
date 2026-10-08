//! Device and stream policy: which microphone a dictation records from, and
//! whether the stream already built is still the one to use.
//!
//! Two rules, both pure, both tested without a device:
//!
//! - [`choose_input_device`] decides between the system default and the
//!   built-in microphone, because a Bluetooth headset costs seconds to open and
//!   drops the user's music while it is open.
//! - [`paused_stream_is_reusable`] decides whether the stream prepared while the
//!   app was idle still matches the device and format a Hotkey just observed.
//!
//! [`PrepareState`], [`should_attempt_prepare`], and [`prepare_state_after`]
//! carry the same idea for the idle-time half: what the recorder has already
//! validated, and what it may retry.

use crate::audio_capture::signal::AudioCaptureError;

/// How a microphone is attached, as far as choosing one to record from cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrophoneTransport {
    BuiltIn,
    Bluetooth,
    Other,
}

/// The input device a dictation records from: the system default, unless the
/// user prefers the built-in microphone and the default is Bluetooth.
///
/// A Bluetooth headset can only record by switching into its call profile.
/// Opening it from cold took 3.4 s on the reference headset against 0.08 s for
/// the built-in microphone, its audio drops to narrowband for as long as the
/// microphone is open, and the user's music drops with it. The built-in
/// microphone has none of those costs. `others` is only listed when the default
/// is Bluetooth, so the common case pays for no device enumeration. With no
/// built-in microphone to fall back to — a desktop Mac — the default stays.
pub fn choose_input_device<D, I>(
    default: D,
    others: impl FnOnce() -> I,
    prefer_built_in: bool,
    transport: impl Fn(&D) -> MicrophoneTransport,
) -> D
where
    I: IntoIterator<Item = D>,
{
    if !prefer_built_in || transport(&default) != MicrophoneTransport::Bluetooth {
        return default;
    }
    others()
        .into_iter()
        .find(|device| transport(device) == MicrophoneTransport::BuiltIn)
        .unwrap_or(default)
}

/// How `device` is attached, asked of the Platform Adapter. Only macOS can
/// tell today; elsewhere every microphone is `Other`, which keeps the default.
pub(super) fn microphone_transport(device: &cpal::Device) -> MicrophoneTransport {
    #[cfg(target_os = "macos")]
    {
        use cpal::traits::DeviceTrait;

        device
            .id()
            .map(|id| crate::macos::microphone_transport(id.id()))
            .unwrap_or(MicrophoneTransport::Other)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = device;
        MicrophoneTransport::Other
    }
}

/// The device to record from on this host, per [`choose_input_device`].
pub(super) fn recording_device(
    host: &cpal::Host,
    prefer_built_in: bool,
) -> Result<cpal::Device, AudioCaptureError> {
    use cpal::traits::HostTrait;

    let default = host
        .default_input_device()
        .ok_or_else(|| AudioCaptureError::new("no default input device is available"))?;
    Ok(choose_input_device(
        default,
        || host.input_devices().into_iter().flatten(),
        prefer_built_in,
        microphone_transport,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InputStreamIdentity {
    pub(super) device_id: Option<cpal::DeviceId>,
    pub(super) sample_format: cpal::SampleFormat,
    pub(super) sample_rate_hz: u32,
    pub(super) channels: u16,
}

/// Whether the paused stream from the previous dictation may serve the next
/// one. Reuse demands every fact about the stream to be unchanged: the
/// recorder must still hold the stream and its ring, and the observed
/// device/format identity must equal the recorded one. Anything less takes
/// the cold-start path and rebuilds all three together (slugtale-op3).
pub(super) fn paused_stream_is_reusable(
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
pub(super) enum PrepareState {
    #[default]
    Unprepared,
    /// Device and format validated and the stream built in a stopped state;
    /// Start only has to play it. See [`DictationRecorder::prepare`] for why a
    /// stopped stream is safe to hold.
    Prepared { identity: InputStreamIdentity },
    /// The last prepare attempt failed with this message. A later prepare may
    /// retry — a missing device can come back.
    Failed(String),
}

/// Whether an idle-time prepare should run at all. A prepared state has
/// nothing left to do, and while a stream is held (recording or paused)
/// preparation must not disturb it; a failure may always be retried — a
/// missing device can come back.
pub(super) fn should_attempt_prepare(state: &PrepareState, stream_held: bool) -> bool {
    !matches!(state, PrepareState::Prepared { .. }) && !stream_held
}

/// Fold a prepare attempt into the next idle-preparation state: success
/// records the device/format identity, failure records why. The recorder
/// replaces its whole state with this result each attempt, so a later
/// success overwrites an earlier failure.
pub(super) fn prepare_state_after(
    outcome: Result<&InputStreamIdentity, &AudioCaptureError>,
) -> PrepareState {
    match outcome {
        Ok(identity) => PrepareState::Prepared {
            identity: identity.clone(),
        },
        Err(error) => PrepareState::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bluetooth_default_gives_way_to_the_built_in_microphone_when_preferred() {
        let transport = |name: &&str| match *name {
            "headset" => MicrophoneTransport::Bluetooth,
            "built-in" => MicrophoneTransport::BuiltIn,
            _ => MicrophoneTransport::Other,
        };
        let devices = || vec!["headset", "usb", "built-in"];

        assert_eq!(
            choose_input_device("headset", devices, true, transport),
            "built-in"
        );
        // The user turned the preference off: their default stands.
        assert_eq!(
            choose_input_device("headset", devices, false, transport),
            "headset"
        );
        // A desktop Mac with no built-in microphone keeps the headset rather
        // than recording from nothing.
        assert_eq!(
            choose_input_device("headset", || vec!["headset", "usb"], true, transport),
            "headset"
        );
    }

    #[test]
    fn a_default_that_is_not_bluetooth_is_kept_without_listing_devices() {
        // Listing devices costs a round trip to the audio system on every
        // press, so only the Bluetooth case may pay for it.
        let transport = |name: &&str| match *name {
            "built-in" => MicrophoneTransport::BuiltIn,
            _ => MicrophoneTransport::Other,
        };
        let never_listed = || -> Vec<&str> { panic!("devices were listed") };

        assert_eq!(
            choose_input_device("usb", never_listed, true, transport),
            "usb"
        );
        assert_eq!(
            choose_input_device("built-in", never_listed, true, transport),
            "built-in"
        );
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
}
