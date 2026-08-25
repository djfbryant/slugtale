# Dictation Segments and Pause Flush

Slugtale splits a dictation into Dictation Segments at each Segment Pause (about five seconds at or below the Dictation Bar's speech level) and runs the full Dictation Workflow once per segment: final transcription, transcript cleanup, immediate insertion, and insertion rescue if insertion fails. This is how Slugtale keeps insertion latency flat for long dictations without ever showing live partial text — every segment is a completed Final Transcription, so ADR-0005's no-live-preview promise still holds, and ADR-0015's immediate insertion happens per segment instead of once per dictation.

The workflow runs on one dedicated worker fed by an ordered channel, which gives three guarantees:

1. **Spoken order.** Segments insert in the order they were spoken however long each decode takes. The worker processes jobs strictly in channel order.
2. **Watermark cuts.** Audio handed to a Pause Flush is cut at the sample watermark recorded when the pause was detected, so a flush never loses or repeats words that straddle the boundary.
3. **Rescue suspends flushes.** After Insertion Rescue fires, later Segment Pauses queue but do not insert until the user resolves the failure, so rescue cannot be buried under new text.

A dictation with no pause is one segment inserted when the user stops — exactly the ADR-0015 behaviour. Usage counts a Counted Segment only when it was inserted or rescued.

Considered options: transcribing the whole dictation on stop (rejected, latency grows with recording length); streaming partial text into the Text Target (rejected by ADR-0005); parallel segment workers for throughput (rejected, ordering then needs reassembly and out-of-order insertion would corrupt the target); inserting from the audio-capture thread (rejected, decode must not stall capture).

The coordination half of this design lives in `main.rs` today; moving it into a dedicated Dictation Runtime module behind a small adapter is tracked separately (slugtale-s2g). This ADR records the behavioural contract that refactor must preserve.
