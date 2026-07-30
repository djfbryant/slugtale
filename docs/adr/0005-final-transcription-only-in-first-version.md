# Final Transcription Only in the First Version

Slugtale's first version will only show or insert final transcriptions. Live partial transcription is deferred so the first version can use simpler ASR integration, simpler UI feedback, and a narrower latency target focused on fast finalization rather than continuous streaming.

This still holds now that a dictation inserts at each Segment Pause rather than only at the end (ADR-0015). A pause flush transcribes speech the user has already finished saying and inserts the finished result; it is several final transcriptions, not a running partial one. Slugtale never revises text it has inserted, and never shows a guess at words still being spoken.
