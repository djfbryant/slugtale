# Transcription and cleanup

Users choose a local engine, set speed and quiet-period insertion, and choose how speech text is cleaned before insertion.

## Sub-features

- `transcription-model`: model download, location, removal, and engine asset setup.
- `transcription-engine`: primary engine and second opinion availability.
- `transcription-speed`: Fast, Balanced, Accurate.
- `transcription-pause`: 2, 3, 5, 8, or 10 seconds of quiet before insertion.
- `transcription-cleanup`: Basic, Clean, Pause breaks, examples, and real saved preferences.

## How to get to it (user POV)

- Open Settings, then `Transcription` or `Text & cleanup`.
- Search `model`, `speed`, `quiet`, or `cleanup` in `Search settings` to reach those same controls through results.
- Under Transcription, use the engine radio choice and model/asset controls shown for the build.

## Driving it with native CUA

Preconditions: doctor passes; clear search before sidebar testing. Preference saves need no model or mic permission. Model/network and live speech paths have additional requirements.

- **Cleanup first proof.** Click `Text & cleanup` (`rail-text`), capture Basic and its example. Click `Clean` (`cleanup-clean-dictation`). Require `Saved. Applies to your next dictation.` and example text `So I think we should ship the beta on Friday. Then collect feedback.` Run `session.mjs capture` for this run and require `transcript_cleanup: clean-dictation`. Click `Pause breaks`; capture the two-line example and persisted `clean-dictation-with-pause-breaks`. Click `Basic`; require `Saved.` and persisted `basic`. Capture each action/result. Actual speech cleanup belongs to the dictation recipe.
- **Speed.** Click `Transcription` (`rail-transcription`), then `Fast`, `Accurate`, and `Balanced` in `Speed versus accuracy`. Capture each Saved message and `speed_profile: fast`, `accurate`, or `balanced`.
- **Quiet interval.** Click `2 s` then `5 s` in `Seconds of silence before inserting` (`pause-2`, `pause-5`). Capture `segment_pause_secs: 2` then `5` and the next-dictation message. Prove actual timing separately in a new dictation.
- **Search entry.** Enter `cleanup`, select a cleanup choice in results, and require the same save and stored value as through the sidebar. Clear search afterward. Do not mark this path passed from the sidebar proof alone.
- **Engine/model setup.** Inspect actual engine states. A radio is named `Make … the primary transcription engine`, with the displayed engine name in place of the ellipsis. Click the observed available radio and capture primary selection and persisted `primary_engine`. For asset work in scope, use the real visible `Download`/`Install` control, capture progress and terminal status, and inspect completed files under `dataDir/models`. Use `Reveal` (`reveal-model-button`) and confirm Finder shows that private path. Use the real Remove/Delete confirmation only for this run's disposable assets; capture file removal and restored missing-model state. Do not copy live model files into the run as a substitute for a download proof.
- **Second opinion.** With two available local engines, click `Automatic` in `Second opinion mode`, capture saved `second_opinion`, then restore `Off`. With fewer engines, capture the actual unavailability state; that is not a successful second-engine inference proof.

## Gotchas

- The first engine can be listed before others; list position is not a selector.
- Unbuilt optional engines and uninstalled assets are different states. Check the real build/features and UI rather than pretending every engine should work.
- Downloads are explicit network operations and can be large. Model-directory entries alone do not prove integrity or inference.
- Saved preferences apply to the next dictation. Pause breaks require engine timing data; examples are static explanatory text.
