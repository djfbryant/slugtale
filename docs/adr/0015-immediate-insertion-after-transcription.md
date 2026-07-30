# Immediate Insertion After Transcription

Slugtale will insert cleaned final transcriptions immediately after dictation completes. It will not show a confirm/edit-before-insert step in v1, because the core workflow is fast dictation into the user's existing text target.

A dictation may now insert more than once. When the user stays quiet for a Segment Pause (about five seconds) the speech so far is transcribed and inserted while recording continues, and later speech is appended after it. Every insertion is still an immediate insertion of a completed final transcription; what changed is how many there are per dictation, not what gets inserted.

Two consequences are accepted deliberately:

- **The caret is not tracked between insertions.** Each pause flush behaves exactly like the single insertion it replaces: it brings the app the user started dictating into back to the front and types at whatever caret that app has now. Slugtale does not detect that the user clicked elsewhere, and does not hold text back when they do. Detecting focus changes would trade this for a quieter failure — a dictation that silently stops inserting — and text landing somewhere visible is easier to notice and undo than words that never arrive.
- **Segments are decoded one at a time.** A short segment must never overtake a long one, so a queue drained by a single worker orders insertions by when they were spoken rather than by how fast each decodes. The cost is that a slow segment delays the next.

If an insertion falls through to the Insertion Rescue, pause flushing stops for the rest of that dictation and the remaining audio is inserted in one piece at the end. Without that, a machine that has not granted Accessibility would overwrite the clipboard and raise a notification every few seconds.
