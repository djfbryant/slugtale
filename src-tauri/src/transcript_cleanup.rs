use serde::{Deserialize, Serialize};

/// How much local Transcript Cleanup runs before insertion (slugtale-kyc).
/// Both modes are deterministic and entirely on-device; neither rewrites
/// meaning, adds generated text, or sends anything anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptCleanupMode {
    /// Whitespace normalization only: today's behaviour.
    #[default]
    Basic,
    /// Whitespace normalization plus conservative filler-word removal for
    /// low-risk hesitations such as "um" (slugtale-5wr).
    CleanDictation,
    /// Clean Dictation plus line breaks at clear pauses between short phrases.
    /// This stays opt-in because a pause is not always a paragraph break.
    CleanDictationWithPauseBreaks,
}

/// A gap this long can be a deliberate separation between short thoughts. It
/// is longer than ordinary breathing, but shorter than the five-second Segment
/// Pause that flushes a Dictation Segment while recording continues.
const PAUSE_LINE_BREAK_MS: u64 = 1_500;

/// Long segments are usually prose. Keeping this small makes the line-break
/// mode conservative when Whisper splits a normal sentence into segments.
const MAX_WORDS_PER_PAUSE_LINE: usize = 8;

/// The hesitation words Filler Cleanup removes when enabled. Deliberately tiny:
/// every word here is one a user almost never wants typed out. Words that can
/// carry meaning — "like", "well", "so", "hmm", "er" — are excluded so the pass
/// prefers false negatives over meaning-changing false positives.
const FILLER_BASES: [&str; 3] = ["um", "uh", "erm"];

/// Whether a token core is a safe hesitation word, including lengthened forms
/// such as "umm" or "uhhh". A hyphenated "uh-uh" (meaning "no") never matches,
/// because the hyphen keeps it from being a bare core.
fn is_filler_core(core: &str) -> bool {
    let lower = core.to_lowercase();
    FILLER_BASES
        .iter()
        .any(|base| filler_variant_of(base, &lower))
}

/// Whether `candidate` is `base` or `base` with its final letter repeated one
/// or more times ("uh" -> "uhh" -> "uhhh"). Bases are non-empty ASCII.
fn filler_variant_of(base: &str, candidate: &str) -> bool {
    let last = base.chars().last().unwrap_or(' ');
    candidate.len() >= base.len()
        && candidate.starts_with(base)
        && candidate[base.len()..].chars().all(|c| c == last)
}

/// Remove conservative filler words from a transcript in one deterministic
/// left-to-right pass over whitespace-separated tokens (slugtale-5wr).
///
/// Rules:
/// - A token whose punctuation-stripped core is a safe filler is dropped.
/// - Sentence-terminal punctuation attached to a dropped filler moves onto the
///   previous kept word ("wait. uh. go" -> "wait. go") rather than vanishing;
///   commas and other soft punctuation die with the filler they set off.
/// - Everything else — capitalization, remaining punctuation, meaningful words
///   such as "like" — rides along untouched. "I like coffee" can never change.
///
/// The output normalizes spaces and tabs while preserving line breaks. It is
/// still ordinary plain text for the existing insertion path.
pub fn remove_filler_words(text: &str) -> String {
    text.split('\n')
        .map(remove_filler_words_from_line)
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

/// Normalize spaces and tabs without flattening intentional line breaks from
/// ASR or generated line breaks from structured cleanup.
pub fn normalize_transcript_whitespace(text: &str) -> String {
    text.split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim_matches('\n')
        .to_string()
}

fn remove_filler_words_from_line(line: &str) -> String {
    let mut kept: Vec<String> = Vec::new();

    for token in line.split_whitespace() {
        let core = alphanumeric_core(token);
        if !is_filler_core(&core) {
            kept.push(token.to_string());
            continue;
        }

        // Preserve sentence ends that rode on the dropped filler.
        let terminal: String = token
            .chars()
            .filter(|c| matches!(c, '.' | '!' | '?'))
            .collect();
        if let Some(previous) = kept.last_mut() {
            if !terminal.is_empty() && !previous.ends_with(['.', '!', '?']) {
                previous.push_str(&terminal);
            }
        }
    }

    kept.join(" ")
}

/// Clean timed ASR segments into plain text, adding a line break only for a
/// clear pause between two short, unfinished phrases. The result is still
/// ordinary text for every existing Text Insertion implementation.
pub fn clean_with_pause_line_breaks(segments: &[crate::TranscriptSegment]) -> String {
    let cleaned: Vec<_> = segments
        .iter()
        .filter_map(|segment| {
            let text = remove_filler_words(&segment.text);
            (!text.is_empty()).then_some((text, segment.start_ms, segment.end_ms))
        })
        .collect();

    let mut text = String::new();
    for (index, (segment, start_ms, _end_ms)) in cleaned.iter().enumerate() {
        if index > 0 {
            let (previous, _previous_start_ms, previous_end_ms) = &cleaned[index - 1];
            if should_insert_pause_line_break(previous, *previous_end_ms, segment, *start_ms) {
                text.push('\n');
            } else {
                text.push(' ');
            }
        }
        text.push_str(segment);
    }
    text
}

fn should_insert_pause_line_break(
    previous: &str,
    previous_end_ms: u64,
    current: &str,
    current_start_ms: u64,
) -> bool {
    let pause_ms = current_start_ms.saturating_sub(previous_end_ms);
    pause_ms >= PAUSE_LINE_BREAK_MS
        && word_count(previous) <= MAX_WORDS_PER_PAUSE_LINE
        && word_count(current) <= MAX_WORDS_PER_PAUSE_LINE
        && !ends_sentence(previous)
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn ends_sentence(text: &str) -> bool {
    matches!(text.trim_end().chars().last(), Some('.' | '!' | '?' | '…'))
}

/// The run of alphanumeric characters inside a token: everything around it is
/// leading or trailing punctuation. A token with no alphanumeric characters
/// yields an empty core, which is never a filler.
fn alphanumeric_core(token: &str) -> String {
    token
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_mode_is_the_default_cleanup() {
        assert_eq!(
            TranscriptCleanupMode::default(),
            TranscriptCleanupMode::Basic
        );
    }

    #[test]
    fn cleanup_mode_persists_as_stable_kebab_strings() {
        for (mode, json) in [
            (TranscriptCleanupMode::Basic, "\"basic\""),
            (TranscriptCleanupMode::CleanDictation, "\"clean-dictation\""),
            (
                TranscriptCleanupMode::CleanDictationWithPauseBreaks,
                "\"clean-dictation-with-pause-breaks\"",
            ),
        ] {
            assert_eq!(serde_json::to_string(&mode).unwrap(), json);
        }
    }

    #[test]
    fn removes_safe_filler_words_and_their_repeats() {
        assert_eq!(
            remove_filler_words("um hello from slugtale"),
            "hello from slugtale"
        );
        assert_eq!(remove_filler_words("um um okay okay"), "okay okay");
        assert_eq!(remove_filler_words("So erm we left"), "So we left");
    }

    #[test]
    fn removes_lengthened_hesitation_forms() {
        assert_eq!(remove_filler_words("ummm okay"), "okay");
        assert_eq!(remove_filler_words("uhhhh right"), "right");
    }

    #[test]
    fn never_touches_meaningful_words_like_like() {
        assert_eq!(remove_filler_words("I like coffee"), "I like coffee");
        assert_eq!(
            remove_filler_words("um I like coffee a lot"),
            "I like coffee a lot"
        );
    }

    #[test]
    fn keeps_the_negative_uh_uh_intact() {
        assert_eq!(
            remove_filler_words("Uh-uh, not that one."),
            "Uh-uh, not that one."
        );
    }

    #[test]
    fn never_recases_words_itself() {
        // A dropped opening filler leaves lowercase text behind; Dictation
        // Workflow's first-segment rule owns capitalization.
        assert_eq!(remove_filler_words("Um we left"), "we left");
        assert_eq!(remove_filler_words("We um left"), "We left");
    }

    #[test]
    fn preserves_sentence_ends_that_ride_on_a_dropped_filler() {
        assert_eq!(remove_filler_words("Wait. Uh. Go now."), "Wait. Go now.");
        assert_eq!(remove_filler_words("So uh? no way"), "So? no way");
    }

    #[test]
    fn commas_set_off_a_filler_and_die_with_it() {
        // The comma before a removed filler stays with the word it followed;
        // only the filler's own soft punctuation disappears.
        assert_eq!(remove_filler_words("I was, um, late."), "I was, late.");
    }

    #[test]
    fn normalizes_whitespace_while_removing_fillers() {
        assert_eq!(
            remove_filler_words("  um   hello\tfrom \n slugtale  "),
            "hello from\nslugtale"
        );
    }

    #[test]
    fn whitespace_normalization_preserves_line_breaks() {
        assert_eq!(
            normalize_transcript_whitespace("  shopping   list \n\t milk and bread  "),
            "shopping list\nmilk and bread"
        );
    }

    #[test]
    fn all_filler_text_cleans_to_empty() {
        assert_eq!(remove_filler_words("um uh"), "");
        assert_eq!(remove_filler_words("   "), "");
    }

    #[test]
    fn tokens_without_alphanumeric_cores_are_never_fillers() {
        assert_eq!(remove_filler_words("-- -- ok"), "-- -- ok");
    }

    #[test]
    fn inserts_a_line_break_for_a_clear_pause_between_short_phrases() {
        let text = clean_with_pause_line_breaks(&[
            crate::TranscriptSegment {
                text: "shopping list".to_string(),
                start_ms: 0,
                end_ms: 800,
            },
            crate::TranscriptSegment {
                text: "milk and bread".to_string(),
                start_ms: 2_400,
                end_ms: 3_100,
            },
        ]);

        assert_eq!(text, "shopping list\nmilk and bread");
    }

    #[test]
    fn keeps_continuous_or_sentence_prose_on_one_line() {
        let continuous = clean_with_pause_line_breaks(&[
            crate::TranscriptSegment {
                text: "This is a normal sentence".to_string(),
                start_ms: 0,
                end_ms: 1_000,
            },
            crate::TranscriptSegment {
                text: "that continues naturally".to_string(),
                start_ms: 1_300,
                end_ms: 2_000,
            },
        ]);
        let sentence_end = clean_with_pause_line_breaks(&[
            crate::TranscriptSegment {
                text: "This sentence is complete.".to_string(),
                start_ms: 0,
                end_ms: 900,
            },
            crate::TranscriptSegment {
                text: "The next one follows.".to_string(),
                start_ms: 2_700,
                end_ms: 3_400,
            },
        ]);

        assert_eq!(
            continuous,
            "This is a normal sentence that continues naturally"
        );
        assert_eq!(
            sentence_end,
            "This sentence is complete. The next one follows."
        );
    }

    #[test]
    fn removes_fillers_before_deciding_pause_line_breaks() {
        let text = clean_with_pause_line_breaks(&[
            crate::TranscriptSegment {
                text: "um groceries".to_string(),
                start_ms: 0,
                end_ms: 500,
            },
            crate::TranscriptSegment {
                text: "uh eggs".to_string(),
                start_ms: 2_200,
                end_ms: 2_600,
            },
        ]);

        assert_eq!(text, "groceries\neggs");
    }
}
