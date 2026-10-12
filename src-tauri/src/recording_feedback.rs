use crate::DictationEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DictationSound {
    Start,
    Stop,
}

/// The observable response of the recording surface to a [`DictationEvent`]: the
/// audible cue to play (if any) and whether the Dictation Bar is now shown. It
/// deliberately carries no transcription text — v1 shows no live transcript
/// (ADR-0014, ADR-0005). Whether the dictation completed or was discarded is not
/// repeated here: the caller already has the event, and that is where the two
/// are told apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingFeedbackEffect {
    pub sound: Option<DictationSound>,
    pub bar_visible: bool,
}

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
        if matches!(event, DictationEvent::Stop | DictationEvent::Cancel) && !self.bar_visible {
            return RecordingFeedbackEffect {
                sound: None,
                bar_visible: false,
            };
        }

        match event {
            DictationEvent::Start => {
                self.bar_visible = true;
                RecordingFeedbackEffect {
                    sound: Some(DictationSound::Start),
                    bar_visible: true,
                }
            }
            DictationEvent::Stop => {
                self.bar_visible = false;
                RecordingFeedbackEffect {
                    sound: Some(DictationSound::Stop),
                    bar_visible: false,
                }
            }
            DictationEvent::Cancel => {
                self.bar_visible = false;
                RecordingFeedbackEffect {
                    sound: None,
                    bar_visible: false,
                }
            }
        }
    }
}

pub fn play_dictation_sound(sound: DictationSound) -> std::io::Result<()> {
    play_sound(sound)
}

#[cfg(target_os = "macos")]
fn play_sound(sound: DictationSound) -> std::io::Result<()> {
    crate::macos::play_dictation_sound(sound)
}

#[cfg(target_os = "windows")]
fn play_sound(sound: DictationSound) -> std::io::Result<()> {
    crate::windows::play_dictation_sound(sound)
}

#[cfg(target_os = "linux")]
fn play_sound(sound: DictationSound) -> std::io::Result<()> {
    crate::linux::play_dictation_sound(sound)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn play_sound(_sound: DictationSound) -> std::io::Result<()> {
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
        assert!(!feedback.bar_visible());
    }

    #[test]
    fn cancelling_dictation_hides_bar_and_discards_without_a_sound() {
        let mut feedback = RecordingFeedback::new();
        feedback.on_event(DictationEvent::Start);

        let effect = feedback.on_event(DictationEvent::Cancel);

        assert_eq!(effect.sound, None);
        assert!(!effect.bar_visible);
        assert!(!feedback.bar_visible());
    }

    #[test]
    fn a_terminal_event_after_the_bar_is_hidden_is_ignored() {
        let mut feedback = RecordingFeedback::new();
        feedback.on_event(DictationEvent::Start);
        feedback.on_event(DictationEvent::Cancel);

        let effect = feedback.on_event(DictationEvent::Stop);

        assert_eq!(effect.sound, None);
        assert!(!effect.bar_visible);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn play_dictation_sound_returns_without_blocking() {
        assert!(play_dictation_sound(DictationSound::Start).is_ok());
    }
}
