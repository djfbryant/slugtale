# Dedicated openWakeWord Detector for Voice Activation

Slugtale will use a dedicated openWakeWord model to detect "Hi Slugtale" in production. The model will accept mono 16 kHz audio and return a score. It will not create a transcript. The Voice Activation listen loop will keep control of microphone ownership, retries, dictation suppression, cooldown, and the final dictation trigger.

The current rolling Whisper detector remains a spike. It proved the listen loop and the Dictation Lifecycle path, but it runs a full speech-to-text model every time speech enters the rolling window. Production builds must not fall back to Whisper when the wake-word model is missing or cannot run. Voice Activation will report itself unavailable instead. This rule keeps idle CPU use clear and testable.

The selected model is a custom openWakeWord classifier for "Hi Slugtale." Training will use the openWakeWord preprocessing model and shared embedding model, then export the classifier as ONNX. The app will run the ONNX models in Rust. It will not ship Python. The model files and their configuration will be pinned by SHA-256 digest, like the other managed local model files.

Slugtale will not bundle or download the wake-word model without a user action. If Voice Activation needs the model, Settings will show **Install wake-word model** beside the Voice Activation setting. That action will download the pinned files into Slugtale's local models directory, stage them, verify their SHA-256 digests, and then make them active. A failed or interrupted download must leave the previous working model unchanged. After installation, detection must work with network access denied.

Slugtale will train and publish its own classifier weights. The openWakeWord code uses the Apache 2.0 license, but its bundled wake-word models use the Creative Commons Attribution-NonCommercial-ShareAlike 4.0 license. Slugtale will not copy those bundled models. The model card for the Slugtale classifier must record the licenses and sources for all training voices, noise, and augmentation data.

The runtime boundary has one data shape. A window of audio enters the detector. The detector returns either a score or an error. The listen loop applies the threshold and cooldown, then emits the existing wake trigger. Transcript variants, fuzzy text matching, and transcript logging do not belong in the production path.

The model cannot leave the experimental feature until one repeatable test records all of these results:

1. False accepts from long recordings of speech, music, and room noise.
2. False rejects from different speakers, microphones, distances, and noise levels.
3. Average and peak CPU use during quiet and active listening.
4. Peak memory use and model load time.
5. Successful detection with network access denied and no attempted remote fallback.

The accepted threshold and the test corpus digest will live with the model card. Audio and transcript content must not enter logs or product history. The test corpus stays outside the product and needs explicit consent and a clear license.

Porcupine was rejected. Its runtime needs a Picovoice account AccessKey, and custom models come from the Picovoice Console. A secret key and account limit do not fit a free app that must work locally after installation. Porcupine also produces platform-specific model files.

The rolling Whisper path was rejected for production because continuous speech-to-text work wastes CPU and competes with dictation. It remains useful only as the completed spike in `slugtale-e95`.

`wakeword-forge` was also considered. It exports a much smaller ONNX model and keeps training audio local by default. It does not yet publish cross-speaker benchmark results, and its own documentation warns that a single-speaker model generalizes poorly. Slugtale needs one bundled model that works for many users, so openWakeWord is the safer current base.

Sources: [openWakeWord model architecture, training, and licensing](https://github.com/dscripka/openWakeWord), [Porcupine runtime and custom-model requirements](https://picovoice.ai/docs/porcupine/), and [wakeword-forge limits and export format](https://github.com/H-Ali13381/wakeword-forge).
