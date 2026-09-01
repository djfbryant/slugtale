use std::fmt;

/// A canonical XDG GlobalShortcuts trigger containing at least one modifier
/// and exactly one xkbcommon keysym.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PortalAccelerator(String);

impl PortalAccelerator {
    /// Converts a stored Tauri/global-hotkey accelerator into a validated
    /// portal trigger.
    ///
    /// The settings frontend emits `Cmd`, `Ctrl`, `Alt`, and `Shift` with bare
    /// letters, digits, or `KeyboardEvent.code` names. Additional aliases
    /// accepted by the current global-hotkey parser remain supported for
    /// compatibility with existing or manually edited settings.
    pub fn from_stored(stored: &str) -> Result<Self, PortalAcceleratorError> {
        parse_stored(stored).map(render)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PortalAccelerator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why a stored accelerator cannot be represented as a portal trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortalAcceleratorError {
    EmptyInput,
    EmptyToken { index: usize },
    UnknownToken { index: usize, token: String },
    ModifierAfterMainKey { index: usize, modifier: String },
    MultipleMainKeys { index: usize, token: String },
    MissingMainKey,
    Modifierless,
}

impl fmt::Display for PortalAcceleratorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => formatter.write_str("stored accelerator is empty"),
            Self::EmptyToken { index } => {
                write!(formatter, "stored accelerator token {} is empty", index + 1)
            }
            Self::UnknownToken { index, token } => write!(
                formatter,
                "unknown stored accelerator token `{token}` at position {}",
                index + 1
            ),
            Self::ModifierAfterMainKey { index, modifier } => write!(
                formatter,
                "modifier `{modifier}` at position {} appears after the main key",
                index + 1
            ),
            Self::MultipleMainKeys { index, token } => write!(
                formatter,
                "main key `{token}` at position {} follows another main key",
                index + 1
            ),
            Self::MissingMainKey => formatter.write_str("stored accelerator is missing a main key"),
            Self::Modifierless => {
                formatter.write_str("stored accelerator must include at least one modifier")
            }
        }
    }
}

impl std::error::Error for PortalAcceleratorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortalModifier {
    Ctrl,
    Alt,
    Shift,
    Logo,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ModifierSet(u8);

impl ModifierSet {
    fn insert(&mut self, modifier: PortalModifier) {
        self.0 |= modifier_bit(modifier);
    }

    fn contains(self, modifier: PortalModifier) -> bool {
        self.0 & modifier_bit(modifier) != 0
    }

    fn is_empty(self) -> bool {
        self.0 == 0
    }
}

fn modifier_bit(modifier: PortalModifier) -> u8 {
    match modifier {
        PortalModifier::Ctrl => 1 << 0,
        PortalModifier::Alt => 1 << 1,
        PortalModifier::Shift => 1 << 2,
        PortalModifier::Logo => 1 << 3,
    }
}

struct ParsedAccelerator {
    modifiers: ModifierSet,
    keysym: &'static str,
}

const MODIFIER_ALIASES: &[(&str, PortalModifier)] = &[
    ("Ctrl", PortalModifier::Ctrl),
    ("Control", PortalModifier::Ctrl),
    ("Alt", PortalModifier::Alt),
    ("Option", PortalModifier::Alt),
    ("Shift", PortalModifier::Shift),
    ("Cmd", PortalModifier::Logo),
    ("Command", PortalModifier::Logo),
    ("Super", PortalModifier::Logo),
    ("CmdOrCtrl", PortalModifier::Ctrl),
    ("CmdOrControl", PortalModifier::Ctrl),
    ("CommandOrCtrl", PortalModifier::Ctrl),
    ("CommandOrControl", PortalModifier::Ctrl),
];

const KEY_ALIASES: &[(&str, &str)] = &[
    ("Backquote", "grave"),
    ("`", "grave"),
    ("Backslash", "backslash"),
    ("\\", "backslash"),
    ("BracketLeft", "bracketleft"),
    ("[", "bracketleft"),
    ("BracketRight", "bracketright"),
    ("]", "bracketright"),
    ("Pause", "Pause"),
    ("PauseBreak", "Pause"),
    ("Comma", "comma"),
    (",", "comma"),
    ("Equal", "equal"),
    ("=", "equal"),
    ("Minus", "minus"),
    ("-", "minus"),
    ("Period", "period"),
    (".", "period"),
    ("Quote", "apostrophe"),
    ("'", "apostrophe"),
    ("Semicolon", "semicolon"),
    (";", "semicolon"),
    ("Slash", "slash"),
    ("/", "slash"),
    ("Backspace", "BackSpace"),
    ("CapsLock", "Caps_Lock"),
    ("Enter", "Return"),
    ("Space", "space"),
    ("Tab", "Tab"),
    ("Delete", "Delete"),
    ("End", "End"),
    ("Home", "Home"),
    ("Insert", "Insert"),
    ("PageDown", "Page_Down"),
    ("PageUp", "Page_Up"),
    ("PrintScreen", "Print"),
    ("ScrollLock", "Scroll_Lock"),
    ("ArrowDown", "Down"),
    ("Down", "Down"),
    ("ArrowLeft", "Left"),
    ("Left", "Left"),
    ("ArrowRight", "Right"),
    ("Right", "Right"),
    ("ArrowUp", "Up"),
    ("Up", "Up"),
    ("NumLock", "Num_Lock"),
    ("NumpadAdd", "KP_Add"),
    ("NumAdd", "KP_Add"),
    ("NumpadPlus", "KP_Add"),
    ("NumPlus", "KP_Add"),
    ("NumpadDecimal", "KP_Decimal"),
    ("NumDecimal", "KP_Decimal"),
    ("NumpadDivide", "KP_Divide"),
    ("NumDivide", "KP_Divide"),
    ("NumpadEnter", "KP_Enter"),
    ("NumEnter", "KP_Enter"),
    ("NumpadEqual", "KP_Equal"),
    ("NumEqual", "KP_Equal"),
    ("NumpadMultiply", "KP_Multiply"),
    ("NumMultiply", "KP_Multiply"),
    ("NumpadSubtract", "KP_Subtract"),
    ("NumSubtract", "KP_Subtract"),
    ("Escape", "Escape"),
    ("Esc", "Escape"),
    ("AudioVolumeDown", "XF86AudioLowerVolume"),
    ("VolumeDown", "XF86AudioLowerVolume"),
    ("AudioVolumeUp", "XF86AudioRaiseVolume"),
    ("VolumeUp", "XF86AudioRaiseVolume"),
    ("AudioVolumeMute", "XF86AudioMute"),
    ("VolumeMute", "XF86AudioMute"),
    ("MediaPlay", "XF86AudioPlay"),
    ("MediaPause", "XF86AudioPause"),
    ("MediaPlayPause", "XF86MediaPlayPause"),
    ("MediaStop", "XF86AudioStop"),
    ("MediaTrackNext", "XF86AudioNext"),
    ("MediaTrackPrev", "XF86AudioPrev"),
    ("MediaTrackPrevious", "XF86AudioPrev"),
];

const LETTER_KEYSYMS: [&str; 26] = [
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s",
    "t", "u", "v", "w", "x", "y", "z",
];

const DIGIT_KEYSYMS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];

const FUNCTION_KEYSYMS: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

const NUMPAD_DIGIT_KEYSYMS: [&str; 10] = [
    "KP_0", "KP_1", "KP_2", "KP_3", "KP_4", "KP_5", "KP_6", "KP_7", "KP_8", "KP_9",
];

fn modifier_for(token: &str) -> Option<PortalModifier> {
    MODIFIER_ALIASES
        .iter()
        .find_map(|(alias, modifier)| token.eq_ignore_ascii_case(alias).then_some(*modifier))
}

fn keysym_for(token: &str) -> Option<&'static str> {
    if let Some(index) = ascii_letter_index(token) {
        return Some(LETTER_KEYSYMS[index]);
    }
    if let Some(index) = ascii_suffix_index(token, "Key", b'a', b'z') {
        return Some(LETTER_KEYSYMS[index]);
    }
    if let Some(index) = ascii_digit_index(token) {
        return Some(DIGIT_KEYSYMS[index]);
    }
    if let Some(index) = ascii_suffix_index(token, "Digit", b'0', b'9') {
        return Some(DIGIT_KEYSYMS[index]);
    }
    if let Some(keysym) = FUNCTION_KEYSYMS
        .iter()
        .copied()
        .find(|keysym| token.eq_ignore_ascii_case(keysym))
    {
        return Some(keysym);
    }
    if let Some(index) = ascii_suffix_index(token, "Numpad", b'0', b'9')
        .or_else(|| ascii_suffix_index(token, "Num", b'0', b'9'))
    {
        return Some(NUMPAD_DIGIT_KEYSYMS[index]);
    }

    KEY_ALIASES
        .iter()
        .find_map(|(alias, keysym)| token.eq_ignore_ascii_case(alias).then_some(*keysym))
}

fn ascii_letter_index(token: &str) -> Option<usize> {
    if token.len() != 1 {
        return None;
    }
    let letter = token.as_bytes()[0].to_ascii_lowercase();
    letter
        .is_ascii_lowercase()
        .then(|| (letter - b'a') as usize)
}

fn ascii_digit_index(token: &str) -> Option<usize> {
    if token.len() != 1 {
        return None;
    }
    let digit = token.as_bytes()[0];
    digit.is_ascii_digit().then(|| (digit - b'0') as usize)
}

fn ascii_suffix_index(token: &str, prefix: &str, first: u8, last: u8) -> Option<usize> {
    let candidate_prefix = token.get(..prefix.len())?;
    if !candidate_prefix.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let suffix = token.get(prefix.len()..)?;
    if suffix.len() != 1 {
        return None;
    }
    let value = suffix.as_bytes()[0].to_ascii_lowercase();
    (first..=last)
        .contains(&value)
        .then(|| (value - first) as usize)
}

fn parse_stored(stored: &str) -> Result<ParsedAccelerator, PortalAcceleratorError> {
    if stored.trim().is_empty() {
        return Err(PortalAcceleratorError::EmptyInput);
    }

    let mut modifiers = ModifierSet::default();
    let mut keysym = None;

    for (index, raw_token) in stored.split('+').enumerate() {
        let token = raw_token.trim();
        if token.is_empty() {
            return Err(PortalAcceleratorError::EmptyToken { index });
        }

        if let Some(modifier) = modifier_for(token) {
            if keysym.is_some() {
                return Err(PortalAcceleratorError::ModifierAfterMainKey {
                    index,
                    modifier: token.to_owned(),
                });
            }
            modifiers.insert(modifier);
            continue;
        }

        if let Some(mapped_keysym) = keysym_for(token) {
            if keysym.is_some() {
                return Err(PortalAcceleratorError::MultipleMainKeys {
                    index,
                    token: token.to_owned(),
                });
            }
            keysym = Some(mapped_keysym);
            continue;
        }

        return Err(PortalAcceleratorError::UnknownToken {
            index,
            token: token.to_owned(),
        });
    }

    let Some(keysym) = keysym else {
        return Err(PortalAcceleratorError::MissingMainKey);
    };
    if modifiers.is_empty() {
        return Err(PortalAcceleratorError::Modifierless);
    }

    Ok(ParsedAccelerator { modifiers, keysym })
}

fn render(parsed: ParsedAccelerator) -> PortalAccelerator {
    const ORDERED_MODIFIERS: [(PortalModifier, &str); 4] = [
        (PortalModifier::Ctrl, "CTRL"),
        (PortalModifier::Alt, "ALT"),
        (PortalModifier::Shift, "SHIFT"),
        (PortalModifier::Logo, "LOGO"),
    ];

    let mut tokens = Vec::with_capacity(ORDERED_MODIFIERS.len() + 1);
    for (modifier, rendered) in ORDERED_MODIFIERS {
        if parsed.modifiers.contains(modifier) {
            tokens.push(rendered);
        }
    }
    tokens.push(parsed.keysym);
    PortalAccelerator(tokens.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const EXPECTED_MODIFIER_ALIASES: &[(&str, &str)] = &[
        ("Ctrl", "CTRL"),
        ("Control", "CTRL"),
        ("Alt", "ALT"),
        ("Option", "ALT"),
        ("Shift", "SHIFT"),
        ("Cmd", "LOGO"),
        ("Command", "LOGO"),
        ("Super", "LOGO"),
        ("CmdOrCtrl", "CTRL"),
        ("CmdOrControl", "CTRL"),
        ("CommandOrCtrl", "CTRL"),
        ("CommandOrControl", "CTRL"),
    ];

    const EXPECTED_FIXED_KEY_ALIASES: &[(&str, &str)] = &[
        ("Backquote", "grave"),
        ("`", "grave"),
        ("Backslash", "backslash"),
        ("\\", "backslash"),
        ("BracketLeft", "bracketleft"),
        ("[", "bracketleft"),
        ("BracketRight", "bracketright"),
        ("]", "bracketright"),
        ("Pause", "Pause"),
        ("PauseBreak", "Pause"),
        ("Comma", "comma"),
        (",", "comma"),
        ("Equal", "equal"),
        ("=", "equal"),
        ("Minus", "minus"),
        ("-", "minus"),
        ("Period", "period"),
        (".", "period"),
        ("Quote", "apostrophe"),
        ("'", "apostrophe"),
        ("Semicolon", "semicolon"),
        (";", "semicolon"),
        ("Slash", "slash"),
        ("/", "slash"),
        ("Backspace", "BackSpace"),
        ("CapsLock", "Caps_Lock"),
        ("Enter", "Return"),
        ("Space", "space"),
        ("Tab", "Tab"),
        ("Delete", "Delete"),
        ("End", "End"),
        ("Home", "Home"),
        ("Insert", "Insert"),
        ("PageDown", "Page_Down"),
        ("PageUp", "Page_Up"),
        ("PrintScreen", "Print"),
        ("ScrollLock", "Scroll_Lock"),
        ("ArrowDown", "Down"),
        ("Down", "Down"),
        ("ArrowLeft", "Left"),
        ("Left", "Left"),
        ("ArrowRight", "Right"),
        ("Right", "Right"),
        ("ArrowUp", "Up"),
        ("Up", "Up"),
        ("NumLock", "Num_Lock"),
        ("NumpadAdd", "KP_Add"),
        ("NumAdd", "KP_Add"),
        ("NumpadPlus", "KP_Add"),
        ("NumPlus", "KP_Add"),
        ("NumpadDecimal", "KP_Decimal"),
        ("NumDecimal", "KP_Decimal"),
        ("NumpadDivide", "KP_Divide"),
        ("NumDivide", "KP_Divide"),
        ("NumpadEnter", "KP_Enter"),
        ("NumEnter", "KP_Enter"),
        ("NumpadEqual", "KP_Equal"),
        ("NumEqual", "KP_Equal"),
        ("NumpadMultiply", "KP_Multiply"),
        ("NumMultiply", "KP_Multiply"),
        ("NumpadSubtract", "KP_Subtract"),
        ("NumSubtract", "KP_Subtract"),
        ("Escape", "Escape"),
        ("Esc", "Escape"),
        ("AudioVolumeDown", "XF86AudioLowerVolume"),
        ("VolumeDown", "XF86AudioLowerVolume"),
        ("AudioVolumeUp", "XF86AudioRaiseVolume"),
        ("VolumeUp", "XF86AudioRaiseVolume"),
        ("AudioVolumeMute", "XF86AudioMute"),
        ("VolumeMute", "XF86AudioMute"),
        ("MediaPlay", "XF86AudioPlay"),
        ("MediaPause", "XF86AudioPause"),
        ("MediaPlayPause", "XF86MediaPlayPause"),
        ("MediaStop", "XF86AudioStop"),
        ("MediaTrackNext", "XF86AudioNext"),
        ("MediaTrackPrev", "XF86AudioPrev"),
        ("MediaTrackPrevious", "XF86AudioPrev"),
    ];

    fn assert_output_grammar(accelerator: &PortalAccelerator) {
        let tokens: Vec<_> = accelerator.as_str().split('+').collect();
        assert!(tokens.len() >= 2, "{}", accelerator.as_str());
        assert!(tokens.iter().all(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        }));

        let (keysym, modifiers) = tokens.split_last().expect("at least two tokens");
        assert!(!["CTRL", "ALT", "SHIFT", "LOGO"].contains(keysym));
        let mut previous_order = None;
        for modifier in modifiers {
            let order = ["CTRL", "ALT", "SHIFT", "LOGO"]
                .iter()
                .position(|known| known == modifier)
                .expect("only canonical portal modifiers are rendered");
            assert!(previous_order.is_none_or(|previous| previous < order));
            previous_order = Some(order);
        }
    }

    fn assert_key_maps(stored_key: &str, expected_keysym: &str) {
        let stored = format!("Ctrl+{stored_key}");
        let expected = format!("CTRL+{expected_keysym}");
        let accelerator = PortalAccelerator::from_stored(&stored).unwrap();
        assert_eq!(accelerator.as_str(), expected, "{stored}");
        assert_eq!(accelerator.to_string(), expected, "{stored}");
        assert_output_grammar(&accelerator);
    }

    fn invert_ascii_case(value: &str) -> String {
        value
            .bytes()
            .map(|byte| {
                if byte.is_ascii_lowercase() {
                    byte.to_ascii_uppercase()
                } else if byte.is_ascii_uppercase() {
                    byte.to_ascii_lowercase()
                } else {
                    byte
                }
            })
            .map(char::from)
            .collect()
    }

    #[test]
    fn common_accelerators_are_canonicalized() {
        let cases = [
            ("Cmd+Shift+D", "SHIFT+LOGO+d"),
            ("CmdOrCtrl+Space", "CTRL+space"),
            ("Super+D", "LOGO+d"),
            ("CommandOrControl+Option+Enter", "CTRL+ALT+Return"),
            ("super+shift+alt+ctrl+ArrowUp", "CTRL+ALT+SHIFT+LOGO+Up"),
            (" control + keyq ", "CTRL+q"),
            ("Ctrl+NumLock", "CTRL+Num_Lock"),
            ("Ctrl+MediaPlayPause", "CTRL+XF86MediaPlayPause"),
        ];

        for (stored, expected) in cases {
            let accelerator = PortalAccelerator::from_stored(stored).unwrap();
            assert_eq!(accelerator.as_str(), expected, "{stored}");
            assert_output_grammar(&accelerator);
        }
    }

    #[test]
    fn modifier_aliases_are_case_insensitive_and_use_linux_semantics() {
        for (alias, expected_modifier) in EXPECTED_MODIFIER_ALIASES {
            for input in [alias.to_string(), invert_ascii_case(alias)] {
                let stored = format!("{input}+D");
                let expected = format!("{expected_modifier}+d");
                assert_eq!(
                    PortalAccelerator::from_stored(&stored).unwrap().as_str(),
                    expected,
                    "{stored}"
                );
            }
        }
    }

    #[test]
    fn duplicate_semantic_modifiers_collapse() {
        assert_eq!(
            PortalAccelerator::from_stored(
                "Ctrl+Control+CmdOrCtrl+CmdOrControl+CommandOrCtrl+CommandOrControl+D"
            )
            .unwrap()
            .as_str(),
            "CTRL+d"
        );
        assert_eq!(
            PortalAccelerator::from_stored("Cmd+Command+Super+D")
                .unwrap()
                .as_str(),
            "LOGO+d"
        );
    }

    #[test]
    fn every_fixed_key_alias_maps_to_the_expected_keysym() {
        for (alias, expected_keysym) in EXPECTED_FIXED_KEY_ALIASES {
            assert_key_maps(alias, expected_keysym);
            assert_key_maps(&invert_ascii_case(alias), expected_keysym);
        }
    }

    #[test]
    fn letter_and_digit_ranges_accept_frontend_and_event_code_forms() {
        for index in 0..26 {
            let upper = char::from(b'A' + index);
            let lower = char::from(b'a' + index).to_string();
            assert_key_maps(&upper.to_string(), &lower);
            assert_key_maps(&format!("Key{upper}"), &lower);
            assert_key_maps(&format!("kEy{}", upper.to_ascii_lowercase()), &lower);
        }

        for digit in 0..10 {
            let digit = digit.to_string();
            assert_key_maps(&digit, &digit);
            assert_key_maps(&format!("Digit{digit}"), &digit);
            assert_key_maps(&format!("dIgIt{digit}"), &digit);
        }
    }

    #[test]
    fn function_and_numpad_digit_ranges_are_complete() {
        for number in 1..=24 {
            let expected = format!("F{number}");
            assert_key_maps(&expected, &expected);
            assert_key_maps(&format!("f{number}"), &expected);
        }

        for digit in 0..10 {
            let expected = format!("KP_{digit}");
            assert_key_maps(&format!("Numpad{digit}"), &expected);
            assert_key_maps(&format!("Num{digit}"), &expected);
            assert_key_maps(&format!("nUmPaD{digit}"), &expected);
        }
    }

    #[test]
    fn bounded_ranges_reject_out_of_range_and_malformed_tokens() {
        for token in ["F0", "F01", "F25", "KeyAA", "Digit10", "Numpad10", "Num10"] {
            assert_eq!(
                PortalAccelerator::from_stored(&format!("Ctrl+{token}")),
                Err(PortalAcceleratorError::UnknownToken {
                    index: 1,
                    token: token.to_owned(),
                })
            );
        }
    }

    #[test]
    fn empty_input_and_components_are_distinct_errors() {
        for input in ["", "   ", "\t\n"] {
            assert_eq!(
                PortalAccelerator::from_stored(input),
                Err(PortalAcceleratorError::EmptyInput)
            );
        }

        for (input, index) in [("+Ctrl+D", 0), ("Ctrl++D", 1), ("Ctrl+D+", 2)] {
            assert_eq!(
                PortalAccelerator::from_stored(input),
                Err(PortalAcceleratorError::EmptyToken { index })
            );
        }
    }

    #[test]
    fn unknown_tokens_preserve_the_trimmed_input_and_position() {
        assert_eq!(
            PortalAccelerator::from_stored("Ctrl+  Banana  "),
            Err(PortalAcceleratorError::UnknownToken {
                index: 1,
                token: "Banana".to_owned(),
            })
        );
        assert_eq!(
            PortalAccelerator::from_stored("Hyper+D"),
            Err(PortalAcceleratorError::UnknownToken {
                index: 0,
                token: "Hyper".to_owned(),
            })
        );
    }

    #[test]
    fn key_cardinality_and_order_errors_are_deterministic() {
        for input in ["Ctrl", "Cmd+Shift"] {
            assert_eq!(
                PortalAccelerator::from_stored(input),
                Err(PortalAcceleratorError::MissingMainKey)
            );
        }
        for input in ["D", "Escape"] {
            assert_eq!(
                PortalAccelerator::from_stored(input),
                Err(PortalAcceleratorError::Modifierless)
            );
        }
        assert_eq!(
            PortalAccelerator::from_stored("Ctrl+D+Alt"),
            Err(PortalAcceleratorError::ModifierAfterMainKey {
                index: 2,
                modifier: "Alt".to_owned(),
            })
        );
        assert_eq!(
            PortalAccelerator::from_stored("Ctrl+D+E"),
            Err(PortalAcceleratorError::MultipleMainKeys {
                index: 2,
                token: "E".to_owned(),
            })
        );
        assert_eq!(
            PortalAccelerator::from_stored("D+E"),
            Err(PortalAcceleratorError::MultipleMainKeys {
                index: 1,
                token: "E".to_owned(),
            })
        );
    }

    #[test]
    fn every_error_variant_has_a_stable_diagnostic() {
        let cases = [
            (
                PortalAcceleratorError::EmptyInput,
                "stored accelerator is empty",
            ),
            (
                PortalAcceleratorError::EmptyToken { index: 1 },
                "stored accelerator token 2 is empty",
            ),
            (
                PortalAcceleratorError::UnknownToken {
                    index: 1,
                    token: "Banana".to_owned(),
                },
                "unknown stored accelerator token `Banana` at position 2",
            ),
            (
                PortalAcceleratorError::ModifierAfterMainKey {
                    index: 2,
                    modifier: "Shift".to_owned(),
                },
                "modifier `Shift` at position 3 appears after the main key",
            ),
            (
                PortalAcceleratorError::MultipleMainKeys {
                    index: 2,
                    token: "E".to_owned(),
                },
                "main key `E` at position 3 follows another main key",
            ),
            (
                PortalAcceleratorError::MissingMainKey,
                "stored accelerator is missing a main key",
            ),
            (
                PortalAcceleratorError::Modifierless,
                "stored accelerator must include at least one modifier",
            ),
        ];

        for (error, expected) in cases {
            assert_eq!(error.to_string(), expected);
            let as_error: &dyn std::error::Error = &error;
            assert!(as_error.source().is_none());
        }
    }

    #[test]
    fn independent_fixed_mapping_expectations_cover_the_production_tables() {
        assert_eq!(MODIFIER_ALIASES.len(), EXPECTED_MODIFIER_ALIASES.len());
        for (alias, _) in MODIFIER_ALIASES {
            assert!(
                EXPECTED_MODIFIER_ALIASES
                    .iter()
                    .any(|(expected, _)| alias.eq_ignore_ascii_case(expected)),
                "missing modifier expectation for {alias}"
            );
        }

        assert_eq!(KEY_ALIASES.len(), EXPECTED_FIXED_KEY_ALIASES.len());
        for (alias, _) in KEY_ALIASES {
            assert!(
                EXPECTED_FIXED_KEY_ALIASES
                    .iter()
                    .any(|(expected, _)| alias.eq_ignore_ascii_case(expected)),
                "missing key expectation for {alias}"
            );
        }
    }

    #[test]
    fn accepted_aliases_have_no_case_insensitive_duplicates() {
        let mut aliases = BTreeSet::new();
        let mut insert = |alias: String| {
            assert!(
                aliases.insert(alias.to_ascii_lowercase()),
                "duplicate accelerator alias: {alias}"
            );
        };

        for (alias, _) in MODIFIER_ALIASES {
            insert((*alias).to_owned());
        }
        for (alias, _) in KEY_ALIASES {
            insert((*alias).to_owned());
        }
        for letter in b'A'..=b'Z' {
            insert(char::from(letter).to_string());
            insert(format!("Key{}", char::from(letter)));
        }
        for digit in 0..10 {
            insert(digit.to_string());
            insert(format!("Digit{digit}"));
            insert(format!("Numpad{digit}"));
            insert(format!("Num{digit}"));
        }
        for function in 1..=24 {
            insert(format!("F{function}"));
        }
    }
}
