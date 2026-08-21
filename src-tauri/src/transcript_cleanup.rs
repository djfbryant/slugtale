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
}

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
/// The output is whitespace-normalized plain text, exactly what the existing
/// insertion path already receives from Basic cleanup.
pub fn remove_filler_words(text: &str) -> String {
    let mut kept: Vec<String> = Vec::new();

    for token in text.split_whitespace() {
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
            "hello from slugtale"
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
}
