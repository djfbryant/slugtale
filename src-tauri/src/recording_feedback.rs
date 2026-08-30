use crate::DictationEvent;

/// An audible cue played at the edges of a dictation (ADR-0014): a start sound
/// when recording begins and a stop sound when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationSound {
    Start,
    Stop,
}

/// What a dictation produces when it ends: a stopped dictation is `Completed` so
/// it can be transcribed; a cancelled dictation is `Discarded` (CONTEXT.md:
/// Dictation Bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationOutcome {
    Completed,
    Discarded,
}

/// The observable response of the recording surface to a [`DictationEvent`]: the
/// audible cue to play (if any), whether the Dictation Bar is now shown, and the
/// session outcome once the dictation has ended. It deliberately carries no
/// transcription text — v1 shows no live transcript (ADR-0014, ADR-0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingFeedbackEffect {
    pub sound: Option<DictationSound>,
    pub bar_visible: bool,
    pub outcome: Option<DictationOutcome>,
}

/// The recording surface state (ADR-0014). Translates dictation lifecycle
/// [`DictationEvent`]s into the audible and visible feedback the user sees:
/// playing start/stop sounds and showing or hiding the Dictation Bar. It is the
/// single source of truth for how Stop and Cancel differ — both hide the bar,
/// but Stop keeps the dictation while Cancel discards it.
pub struct RecordingFeedback {
    bar_visible: bool,
}

impl Default for RecordingFeedback {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordingFeedback {
    pub fn new() -> Self {
        Self { bar_visible: false }
    }

    pub fn bar_visible(&self) -> bool {
        self.bar_visible
    }

    pub fn on_event(&mut self, event: DictationEvent) -> RecordingFeedbackEffect {
        // Stop and Cancel only matter while a dictation is on screen. Ignoring
        // them once the bar is hidden keeps a stray terminal event (e.g. a
        // hold-mode key release arriving after Escape) from replaying a sound or
        // re-ending the session.
        if matches!(event, DictationEvent::Stop | DictationEvent::Cancel) && !self.bar_visible {
            return RecordingFeedbackEffect {
                sound: None,
                bar_visible: false,
                outcome: None,
            };
        }

        match event {
            DictationEvent::Start => {
                self.bar_visible = true;
                RecordingFeedbackEffect {
                    sound: Some(DictationSound::Start),
                    bar_visible: true,
                    outcome: None,
                }
            }
            DictationEvent::Stop => {
                self.bar_visible = false;
                RecordingFeedbackEffect {
                    sound: Some(DictationSound::Stop),
                    bar_visible: false,
                    outcome: Some(DictationOutcome::Completed),
                }
            }
            DictationEvent::Cancel => {
                self.bar_visible = false;
                RecordingFeedbackEffect {
                    sound: None,
                    bar_visible: false,
                    outcome: Some(DictationOutcome::Discarded),
                }
            }
        }
    }
}

/// Play the audible cue for a dictation edge (ADR-0014) through the OS sound
/// service. Like [`crate::open_in_file_manager`], the spawned helper returns
/// immediately so the recording lifecycle is never blocked waiting on audio.
pub fn play_dictation_sound(sound: DictationSound) -> std::io::Result<()> {
    play_sound(sound)
}

#[cfg(target_os = "macos")]
fn play_sound(sound: DictationSound) -> std::io::Result<()> {
    // The afplay call lives in the platform adapter (ADR-0021) so this module
    // stays free of OS bindings, matching the Windows and Linux arms below.
    crate::macos::play_dictation_sound(sound)
}

#[cfg(target_os = "windows")]
fn play_sound(sound: DictationSound) -> std::io::Result<()> {
    // The Win32 call lives in the platform adapter (ADR-0021) so this module
    // stays free of OS bindings.
    crate::windows::play_dictation_sound(sound)
}

#[cfg(target_os = "linux")]
fn play_sound(sound: DictationSound) -> std::io::Result<()> {
    // The XDG sound-theme call lives in the platform adapter (ADR-0021) so this
    // module stays free of OS specifics, matching the Windows arm above.
    crate::linux::play_dictation_sound(sound)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn play_sound(_sound: DictationSound) -> std::io::Result<()> {
    // Other platforms get audible feedback once their Platform Adapter lands
    // (ADR-0021); the recording lifecycle stays platform-agnostic until then.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DictationEvent;

    #[test]
    fn starting_dictation_plays_start_sound_and_shows_bar() {
        let mut feedback = RecordingFeedback::new();

        let effect = feedback.on_event(DictationEvent::Start);

        assert_eq!(effect.sound, Some(DictationSound::Start));
        assert!(effect.bar_visible);
        assert!(feedback.bar_visible());
    }

    #[test]
    fn stopping_dictation_plays_stop_sound_hides_bar_and_completes() {
        let mut feedback = RecordingFeedback::new();
        feedback.on_event(DictationEvent::Start);

        let effect = feedback.on_event(DictationEvent::Stop);

        assert_eq!(effect.sound, Some(DictationSound::Stop));
        assert!(!effect.bar_visible);
        assert_eq!(effect.outcome, Some(DictationOutcome::Completed));
        assert!(!feedback.bar_visible());
    }

    #[test]
    fn cancelling_dictation_hides_bar_and_discards_without_a_sound() {
        let mut feedback = RecordingFeedback::new();
        feedback.on_event(DictationEvent::Start);

        let effect = feedback.on_event(DictationEvent::Cancel);

        assert_eq!(effect.sound, None);
        assert!(!effect.bar_visible);
        assert_eq!(effect.outcome, Some(DictationOutcome::Discarded));
        assert!(!feedback.bar_visible());
    }

    #[test]
    fn a_terminal_event_after_the_bar_is_hidden_is_ignored() {
        // Cancel (e.g. Escape) followed by a stray Stop from a hold-mode key
        // release must not replay the stop sound or re-complete the session.
        let mut feedback = RecordingFeedback::new();
        feedback.on_event(DictationEvent::Start);
        feedback.on_event(DictationEvent::Cancel);

        let effect = feedback.on_event(DictationEvent::Stop);

        assert_eq!(effect.sound, None);
        assert!(!effect.bar_visible);
        assert_eq!(effect.outcome, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn play_dictation_sound_returns_without_blocking() {
        assert!(play_dictation_sound(DictationSound::Start).is_ok());
    }
}
