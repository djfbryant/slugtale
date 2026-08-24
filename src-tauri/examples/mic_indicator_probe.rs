//! Proves, against real macOS, that building a CoreAudio input stream does not
//! activate the microphone until the stream plays (slugtale-g1o.3).
//!
//! Evidence: `kAudioDevicePropertyDeviceIsRunningSomewhere` on the default
//! input device. This is the property that reports whether any process has the
//! device running — the same underlying state the system's microphone
//! indicator reflects. The probe asserts it stays false while a stream is
//! built but stopped, and flips true once it plays.
//!
//! Usage:
//!   cargo run --example mic_indicator_probe

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectPropertyAddress,
    kAudioDevicePropertyDeviceIsRunningSomewhere,
    kAudioHardwarePropertyDefaultInputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
};
use slugtale_lib::{AudioRecorder, CpalAudioRecorder};
use std::ptr::NonNull;
use std::time::Duration;

fn main() {
    let device_id = default_input_device_id().expect("no default input device");

    let quiet = device_is_running_somewhere(device_id);
    println!("baseline (no stream): running={quiet}");
    assert!(
        !quiet,
        "the microphone is already running before we touch it"
    );

    let mut recorder = CpalAudioRecorder::new();
    // Builds the input stream but never plays it — what idle-time prepare does.
    recorder.prepare().expect("prepare capture");
    std::thread::sleep(Duration::from_millis(500));

    let built = device_is_running_somewhere(device_id);
    println!("after building a stopped stream: running={built}");
    assert!(
        !built,
        "building a stopped stream activated the microphone; \
         idle-time preparation must stay on the Hotkey path"
    );

    recorder.start().expect("start capture");
    std::thread::sleep(Duration::from_millis(500));
    let playing = device_is_running_somewhere(device_id);
    println!("while recording: running={playing}");
    assert!(
        playing,
        "sanity check failed: a playing stream did not run the device"
    );

    let _ = recorder.cancel();
    std::thread::sleep(Duration::from_millis(500));
    let after = device_is_running_somewhere(device_id);
    println!("after cancel: running={after}");

    println!("ok: a built-but-stopped stream does not activate the microphone");
}

/// The default input device's `AudioObjectID`, read straight from the HAL so
/// the property query below targets exactly the device cpal will use.
fn default_input_device_id() -> Option<u32> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDefaultInputDevice,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    let mut device_id = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            kAudioObjectSystemObject as u32,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut device_id).cast(),
        )
    };
    (status == 0 && device_id != 0).then_some(device_id)
}

fn device_is_running_somewhere(device_id: u32) -> bool {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioDevicePropertyDeviceIsRunningSomewhere,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };

    let mut running: u32 = 0;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            device_id,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut running).cast(),
        )
    };

    status == 0 && running != 0
}
