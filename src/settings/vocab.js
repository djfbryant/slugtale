    // Vocabularies the Settings window and the Dictation Bar both spell the same
    // way. Each list lives here once and is consumed everywhere — the segmented
    // controls on the Dictation Bar pane, the preferences that save them, and the
    // bar window that paints the live choice — so a value cannot be spelled one
    // way on screen and another in a save, or land on a pane the bar never shows.

    // Order matters: centre is the default and the first fallback the bar paints.
    const BAR_POSITIONS = ["bottom-center", "bottom-left", "bottom-right"];
    const ACCENT_COLORS = ["red", "amber", "green", "blue", "violet", "graphite"];
    const SECOND_OPINION_MODES = ["off", "automatic"];
