# Dictation Segments and Pause Flush

Slugtale splits a dictation into Dictation Segments at each Segment Pause (at or below the Dictation Bar's speech level for the configured number of seconds) and runs the full Dictation Workflow once per segment: final transcription, transcript cleanup, immediate insertion, and insertion rescue if insertion fails. This is how Slugtale keeps insertion latency flat for long dictations without ever showing live partial text — every segment is a completed Final Transcription, so ADR-0005's no-live-preview promise still holds, and ADR-0015's immediate insertion happens per segment instead of once per dictation.

The Segment Pause length is a user setting: two to ten seconds, five by default, the length v1 shipped with. It is stored in the Settings File as a plain number of seconds and rejected at the save boundary outside that range, so an invalid save changes neither the file nor the running runtime. A hand-edited value is clamped into the range when read, so it can never arm a detector with a wild length. A dictation already in progress keeps the length it started with; the next one picks up the change. The pause still counts only after the user has said something, so opening silence never flushes an empty segment.

The workflow runs on one dedicated worker fed by an ordered channel, which gives three guarantees:

1. **Spoken order.** Segments insert in the order they were spoken however long each decode takes. The worker processes jobs strictly in channel order.
2. **Watermark cuts.** Audio handed to a Pause Flush is cut at the sample watermark recorded when the pause was detected, so a flush never loses or repeats words that straddle the boundary.
3. **Rescue suspends flushes.** After Insertion Rescue fires, later Segment Pauses queue but do not insert until the user resolves the failure, so rescue cannot be buried under new text.

A dictation with no pause is one segment inserted when the user stops — exactly the ADR-0015 behaviour. Usage counts a Counted Segment only when it was inserted or rescued.

Considered options: transcribing the whole dictation on stop (rejected, latency grows with recording length); streaming partial text into the Text Target (rejected by ADR-0005); parallel segment workers for throughput (rejected, ordering then needs reassembly and out-of-order insertion would corrupt the target); inserting from the audio-capture thread (rejected, decode must not stall capture).

The coordination half of this design now lives in the Dictation Runtime module, beside the trigger that queues its work, and `main.rs` holds wiring only. That move is complete (slugtale-s2g), and the three guarantees above are the contract it had to keep. The small adapter this ADR once pointed at is gone: the Dictation Host is the runtime's host, so the microphone cut, the Dictation Workflow, and the Dictation Bar hide are all answers one module already had.
