# Security Policy

## Supported Versions

Slugtale is currently pre-release software. Security fixes are targeted at the
current `main` branch only until versioned releases begin.

## Reporting a Vulnerability

Please do not open a public issue for an active vulnerability.

Use GitHub's private vulnerability reporting for this repository when available.
If private reporting is not enabled, contact the repository owner privately and
include:

- A clear description of the issue.
- Steps to reproduce it.
- The affected operating system and Slugtale commit or version.
- Any proof-of-concept code, logs, or screenshots needed to verify the report.

We will acknowledge valid reports as soon as practical, investigate the impact,
and publish a fix or mitigation before public disclosure when the issue affects
users.

## Current Data Handling

Slugtale is designed as a local-first dictation app.

At the time of this policy:

- Captured audio is processed locally.
- Transcriptions are not stored as dictation history.
- No telemetry, analytics, or remote transcription service is used.
- The app may download a local speech model during setup.
- Non-secret preferences are stored locally in the app settings file.
- Usage counts are opt-in, off by default, and stay on the machine. Nothing is
  written until the user turns them on in Settings, and turning them off deletes
  the usage file.
- A daily usage record contains only a local date, a dictation count, a word
  count, and a speaking duration in seconds. It contains no transcription text,
  no audio, no application names, and nothing that identifies what was dictated.
- Time saved is calculated when it is shown and is never stored.
- The typing challenges that measure typing speed run entirely on the machine.
  Their passages ship with the app, and only the resulting words-per-minute
  number is stored, in the settings file.

Dictation history remains absent: no transcript log, no audio archive, and no
record of which application was dictated into.

If future work changes these data-handling guarantees, the security policy and
user-facing documentation should be updated in the same change.
