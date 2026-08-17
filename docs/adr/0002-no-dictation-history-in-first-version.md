# No Dictation History in the First Version

Slugtale will not persist dictation history in the first version: no transcript log, no audio archive, and no target-application history beyond transient runtime state needed to complete insertion. This preserves the local-only privacy promise in its strictest form and keeps storage behavior easy to explain, while leaving a future opt-in history feature possible.

Daily usage counts without transcript text are not dictation history. See ADR-0025.
