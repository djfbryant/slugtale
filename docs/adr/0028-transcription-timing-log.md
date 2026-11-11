# Always-On Local Transcription Timing Log

Slugtale writes one Transcription Timing line per Dictation Segment to a local file beside the Usage File. Each line names the engine and model, the audio length, the transcription time, and a character count. It holds no audio and no transcription text, so it is not Dictation History (ADR-0002) and not telemetry (ADR-0019).

The log is always on: it is not gated by the diagnostic logging setting or by Usage storage, because a slow dictation must be explainable on the machine where it happened without the user having opted in first. A failed write is skipped and never fails a dictation.

Considered options: adding timing only to the opt-in Local Diagnostic Log (rejected, the slow dictation is usually the one before the user turns logging on); storing transcript text or audio beside the timing (rejected, that is Dictation History); sending timing off the machine (rejected, ADR-0019).

Format: the file is `transcription-timing.jsonl` in the app config dir. Each line is one JSON object with keys `engine`, `model_id`, `audio_ms`, `latency_ms`, `characters`. No rotation or cap; delete the file to clear it. See issue #159.
