# Immediate Insertion After Transcription

Slugtale will insert cleaned final transcriptions immediately after dictation completes. It will not show a confirm/edit-before-insert step in v1, because the core workflow is fast dictation into the user's existing text target.

A dictation may now insert more than once. When the user stays quiet for a Segment Pause (about five seconds) the speech so far is transcribed and inserted while recording continues, and later speech is appended after it. Every insertion is still an immediate insertion of a completed final transcription; what changed is how many there are per dictation, not what gets inserted.

Two consequences are accepted deliberately:

- **The caret is not tracked between insertions.** Each pause flush behaves exactly like the single insertion it replaces: it brings the app the user started dictating into back to the front and types at whatever caret that app has now. Slugtale does not detect that the user clicked elsewhere, and does not hold text back when they do. Detecting focus changes would trade this for a quieter failure — a dictation that silently stops inserting — and text landing somewhere visible is easier to notice and undo than words that never arrive.
- **Segments are decoded one at a time.** A short segment must never overtake a long one, so a queue drained by a single worker orders insertions by when they were spoken rather than by how fast each decodes. The cost is that a slow segment delays the next.

## The text target is the application, and it is checked immediately before typing

Activation alone is not enough. A decode can take seconds, and in that time the user can switch from an email to a chat. So the target app is activated again and then **verified** immediately before the first keystroke. If the app cannot be confirmed — the user left it, or the system will not bring it back — the insertion fails like any other failed insertion and ADR-0016's Insertion Rescue preserves the transcription on the clipboard. The words are never typed into whatever happens to be in front.

A dictation that captured no target at all never types. Rescue is the honest answer, because typing into an unidentified app is how words reach a place the user never chose.

The accepted limit: **the pinned target is an application, not a text field.** Slugtale aims at the app the user began dictating into and types at whatever field that app has focused when the words land, so a user who deliberately moves to another field of the same app while a long segment decodes gets their words there. Naming the exact field would mean tracking focus inside every target application, which is not knowable from outside it; an application boundary is the strongest identity Slugtale can honestly hold and check. The visible consequence — text in the wrong field of the right app — is the user's own undo, as above.

If an insertion falls through to the Insertion Rescue, pause flushing stops for the rest of that dictation and the remaining audio is inserted in one piece at the end. Without that, a machine that has not granted Accessibility would overwrite the clipboard and raise a notification every few seconds.
