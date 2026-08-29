# Zero-cost distribution through GitHub Releases

ADR-0022 supersedes ADR-0020. Slugtale ships packaged apps, starting with macOS.
The project does not require a paid signing account or paid CI.

## Decision

GitHub Releases hosts each tagged release and its updater files.

GitHub is free versioned hosting. The hosting location does not change
Gatekeeper or SmartScreen prompts.

The macOS build uses ad hoc signing when no signing identity is set. It is not
notarized. macOS can show Gatekeeper prompts for these builds.

Signing stays optional. The build uses signing secrets when they exist and
builds unsigned artifacts when they do not. Apple notarization and Windows
Trusted Signing can be enabled later through secrets and configuration. The
project does not require either paid service.

Release builds stay local while the repository is private. GitHub-hosted macOS
runners cost more than the free allowance supports. A public repository or an
approved budget can change this decision.

Settings checks for a new version only after the user selects **Check now**.
The Tauri updater reads `latest.json`. Slugtale does not download, install, or
restart into the update.

If a new version exists, Settings offers **Open release page**. A Rust command
opens only `https://github.com/djfbryant/slugtale/releases/latest`. The webview
cannot choose the URL. The webview has no updater or opener permission.

This manual flow protects local builds and forks. An upstream app can have a
different macOS signing identity. Replacing the current app with that build can
make macOS ask for Microphone and Accessibility access again.

Slugtale keeps the updater endpoint, public key, and signed updater artifacts.
The endpoint supports version checks. The key and artifacts leave room for a
later install flow. A future install flow needs proof that it preserves macOS
privacy grants. The current check does not download or verify an artifact.

macOS releases use Metal. Windows and Linux work must keep a CPU fallback when
they add hardware acceleration.

## Consequences

Users must open the release page and choose how to update.

Source builds and forks cannot install an upstream Slugtale bundle from
Settings. Their maintainers can rebuild and install with the same bundle ID and
signing identity.
