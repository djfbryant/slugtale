# Opt-In Local Usage Aggregates

Slugtale offers opt-in Usage: daily counts of dictations, words, and speaking duration stored on the machine, with Time Saved computed from a Typing Baseline. This is not Dictation History (ADR-0002) and not telemetry (ADR-0019). Counts live in a Usage File separate from the Settings File. The Typing Baseline lives in the Settings File so it survives turning Usage off. Usage updates never delay or fail the Dictation Workflow: a Counted Segment is handed to a background writer after it has already reached the text target, and a failed write is skipped rather than reported.

Considered options: per-dictation records (rejected, that is history); default-on collection (rejected, the user chooses to store); showing numbers on the Pill or tray (rejected, Usage is a Settings section only); a default typing speed when none is measured (rejected, Time Saved is left as a hole rather than invented).
