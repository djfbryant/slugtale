# Settings Access with Gated Dictation

Slugtale will allow users into settings even when setup is incomplete, but dictation will remain unavailable until microphone permission, text insertion permission, a configured hotkey, a downloaded local model, and a transcription engine that can actually run are all ready. This keeps access grants under user control while making runtime behavior explicit and predictable.

The engine gate was added later (slugtale-bre). Once engines became plural and opt-in at compile time, a downloaded model stopped implying a working transcriber: a build compiled without that engine's Cargo feature has the weights on disk and nothing able to decode them, and would otherwise report ready and then fail at the moment the user spoke.
