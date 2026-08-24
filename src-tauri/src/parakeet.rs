//! NVIDIA Parakeet TDT v2 0.6B as a Transcription Engine (slugtale-vjs.1).
//!
//! Parakeet is the second entirely on-device engine behind the Transcription
//! Engine boundary. It exists so the Second Opinion router has something to ask
//! when Whisper's transcript looks wrong, and so a user who prefers it can make
//! it the primary engine once benchmark slugtale-9dv settles the ordering.
//!
//! Four things about this module are deliberate and worth reading before
//! changing it.
//!
//! **The provider type is unconditional; only inference is feature-gated.**
//! Settings has to be able to say *why* Parakeet is unavailable on a build
//! compiled without `local-parakeet-runtime`, and it cannot say that about a
//! type that does not exist. So [`ParakeetProvider`] compiles on every platform
//! and every feature set, and reports [`EngineUnavailable::RuntimeNotBuilt`]
//! when the ONNX Runtime toolchain was left out.
//!
//! **Installation is explicit and pinned; inference never touches the network.**
//! The weights are 631 MiB of NVIDIA's model that Slugtale does not bundle and
//! may not silently fetch (ADR-0010, ADR-0001). They arrive only through
//! [`install_parakeet_assets`], driven by a user action in Settings, against a
//! pinned upstream revision and a SHA-256 digest per file. After that,
//! [`ParakeetProvider::transcribe`] reads local files and nothing else — the
//! network-denied test in slugtale-vjs.5 depends on there being no lazy fetch
//! anywhere on this path.
//!
//! **Nothing here is allowed to observe user content.** No transcript, no audio
//! sample, and no confidence value derived from either is printed, logged, or
//! put in an error string. Every error this module produces describes the
//! machine, the build, or the installed files. A test at the bottom of the file
//! enforces the absence of print macros so a debugging session cannot leave one
//! behind.
//!
//! **Parakeet reports no confidence, and that is not the same as low
//! confidence.** The TDT decoder does score its tokens, but `parakeet-rs` 0.3.6
//! throws the scores away: its `TimedToken` carries only `text`, `start`, and
//! `end`. So this provider returns [`crate::EngineConfidence::unreported`]
//! rather than a number invented from token count or duration, and the Second
//! Opinion router escalates *from* Parakeet on the transcript anomaly rules
//! instead of on a threshold. Revisit if the crate starts exposing per-token
//! log-probabilities.

use crate::{
    AsrError, CapturedAudio, DownloadProgress, EngineAvailability, EngineMetadata,
    EngineTranscription, EngineUnavailable, ModelDownloader, ModelError, TranscriptionEngine,
    TranscriptionProvider,
};
#[cfg(feature = "local-parakeet-runtime")]
use crate::{EngineConfidence, FinalTranscription};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// The engine this module provides.
pub const PARAKEET_ENGINE: TranscriptionEngine = TranscriptionEngine::Parakeet;

/// The upstream model, as NVIDIA publishes it. Slugtale installs an ONNX export
/// of these weights rather than the original NeMo checkpoint, but the identity
/// Settings shows the user is NVIDIA's, because that is whose model it is and
/// whose licence applies.
pub const PARAKEET_MODEL_ID: &str = "nvidia/parakeet-tdt-0.6b-v2";

/// The pinned upstream revision of the ONNX export, as `repo@commit`.
///
/// This is a commit hash and never a branch. A floating `main` would let the
/// artefact behind the pinned digests change under us, which both breaks the
/// digests and quietly changes what the user installed after they consented to
/// a specific model.
pub const PARAKEET_REVISION: &str =
    "istupakov/parakeet-tdt-0.6b-v2-onnx@0bbb45a3365852604aef28b538a8f066f4ccaa85";

/// The commit the assets are fetched at. Split out from [`PARAKEET_REVISION`]
/// because it goes into the download URL, where the `repo@` prefix would not.
const PARAKEET_REVISION_COMMIT: &str = "0bbb45a3365852604aef28b538a8f066f4ccaa85";

const PARAKEET_REPO: &str = "istupakov/parakeet-tdt-0.6b-v2-onnx";

/// Where a user can go and look at exactly what Slugtale installs, at the
/// pinned commit rather than at whatever the repository holds today.
pub const PARAKEET_SOURCE_URL: &str = "https://huggingface.co/istupakov/parakeet-tdt-0.6b-v2-onnx/tree/0bbb45a3365852604aef28b538a8f066f4ccaa85";

/// NVIDIA released Parakeet TDT 0.6B v2 under CC BY 4.0, which is an
/// attribution licence: Slugtale may use it commercially and offline, but must
/// credit NVIDIA, link the licence, and state what was changed. Those three
/// obligations are the reason [`EngineMetadata`] has `attribution` and
/// `modifications` fields at all.
pub const PARAKEET_LICENSE: &str = "CC BY 4.0";
pub const PARAKEET_LICENSE_URL: &str = "https://creativecommons.org/licenses/by/4.0/";

pub const PARAKEET_ATTRIBUTION: &str =
    "Speech recognition by NVIDIA Parakeet TDT 0.6B v2 (© NVIDIA Corporation), used under CC BY 4.0.";

/// The CC BY 4.0 "indicate if changes were made" clause. Slugtale does not train
/// or fine-tune the weights; the changes are the ONNX export and the int8
/// quantisation carried out upstream, which Slugtale installs as-is.
pub const PARAKEET_MODIFICATIONS: &str = concat!(
    "Not the original NeMo checkpoint: exported to ONNX and quantised to int8 upstream ",
    "(istupakov/parakeet-tdt-0.6b-v2-onnx). Slugtale installs those artefacts unmodified ",
    "and does not train, fine-tune, or otherwise alter the weights."
);

/// The directory name the assets are installed under, inside Slugtale's models
/// directory. A subdirectory rather than loose files because `parakeet-rs`
/// loads a model *directory* and picks the encoder, decoder, and vocabulary out
/// of it by filename; the Whisper `.bin` sitting alongside them would be noise.
pub const PARAKEET_ASSET_DIR_NAME: &str = "parakeet-tdt-0.6b-v2";

/// One installed file: its name, its exact size, and the SHA-256 digest it must
/// hash to before it is allowed to become part of the installed model.
///
/// Size and digest are both pinned because they catch different failures. The
/// size catches a truncated transfer immediately and for free; the digest
/// catches a complete but wrong or tampered file, and is the one that actually
/// decides whether the bytes are trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParakeetAsset {
    pub filename: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

impl ParakeetAsset {
    /// The pinned download URL. Built from the commit, never from a branch, so
    /// re-running an install a year from now fetches the same bytes.
    pub fn download_url(&self) -> String {
        format!(
            "https://huggingface.co/{PARAKEET_REPO}/resolve/{PARAKEET_REVISION_COMMIT}/{}",
            self.filename
        )
    }
}

/// Every file the Parakeet engine needs on disk, at the pinned revision.
///
/// These are the **int8** artefacts, not the full-precision ones. The fp32
/// encoder is 2.4 GiB of external weights; on the 8 GB reference machine that is
/// a download most users would abandon and a resident memory cost that would
/// crowd out Whisper. The int8 export is 631 MiB in total and is what
/// `parakeet-rs` is exercised against upstream. That choice is a *modification*
/// under CC BY 4.0, which is why [`PARAKEET_MODIFICATIONS`] states it.
///
/// The order matters a little: the vocabulary is tiny and comes first, so a
/// mistyped asset directory or a read-only disk fails in a second rather than
/// after a 600 MiB download.
pub const PARAKEET_ASSETS: [ParakeetAsset; 3] = [
    ParakeetAsset {
        filename: "vocab.txt",
        bytes: 9_384,
        sha256: "ec182b70dd42113aff6c5372c75cac58c952443eb22322f57bbd7f53977d497d",
    },
    ParakeetAsset {
        filename: "decoder_joint-model.int8.onnx",
        bytes: 8_998_286,
        sha256: "a449f49acd68979d418651dd2dcb737cc0f1bf0225e009e29ee326354edbf7d3",
    },
    ParakeetAsset {
        filename: "encoder-model.int8.onnx",
        bytes: 652_184_014,
        sha256: "3e0581fda6ab843888b51e56d7ee78b6d5bc3237ec113af1f732d1d5286aa155",
    },
];

/// How much disk a complete install takes, for the Settings copy and for the
/// aggregate download progress bar.
pub fn parakeet_total_bytes() -> u64 {
    parakeet_manifest_total_bytes(&PARAKEET_ASSETS)
}

fn parakeet_manifest_total_bytes(manifest: &[ParakeetAsset]) -> u64 {
    manifest.iter().map(|asset| asset.bytes).sum()
}

/// Where the Parakeet assets live for a given models directory.
pub fn parakeet_asset_dir(model_dir: &Path) -> PathBuf {
    model_dir.join(PARAKEET_ASSET_DIR_NAME)
}

/// What is installed right now. Non-content by construction: filenames and byte
/// counts only, so this is safe to log and to render in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParakeetAssetStatus {
    pub dir: PathBuf,
    /// Every pinned file is present at its expected size.
    pub present: bool,
    /// Bytes on disk for the files that are present, for a partial-install
    /// progress read-out.
    pub installed_bytes: u64,
    /// The pinned files that are absent or the wrong size, so Settings can say
    /// what an install would still have to fetch.
    pub missing: Vec<&'static str>,
}

/// Read the installed state from the filesystem.
///
/// This checks presence and exact size, not digests. Re-hashing 631 MiB is a
/// multi-second read, and it is the *install* that decides whether bytes are
/// trusted (see [`install_parakeet_assets`]); this function only has to notice
/// that a file went missing or was truncated afterwards. Callers who want the
/// full guarantee back call [`verify_parakeet_assets`].
pub fn parakeet_asset_status(asset_dir: &Path) -> ParakeetAssetStatus {
    parakeet_asset_status_for_manifest(asset_dir, &PARAKEET_ASSETS)
}

fn parakeet_asset_status_for_manifest(
    asset_dir: &Path,
    manifest: &[ParakeetAsset],
) -> ParakeetAssetStatus {
    let mut installed_bytes = 0;
    let mut missing = Vec::new();

    for asset in manifest {
        if asset_file_is_installed(asset_dir, asset) {
            installed_bytes += asset.bytes;
        } else {
            missing.push(asset.filename);
        }
    }

    ParakeetAssetStatus {
        dir: asset_dir.to_path_buf(),
        present: missing.is_empty(),
        installed_bytes,
        missing,
    }
}

fn asset_file_is_installed(asset_dir: &Path, asset: &ParakeetAsset) -> bool {
    asset_dir
        .join(asset.filename)
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == asset.bytes)
}

/// Re-hash every installed file against its pinned digest.
///
/// Deliberately not on the dictation path — this reads the whole 631 MiB. It
/// exists for the explicit "verify installation" action and for the integrity
/// tests, so a user who suspects a corrupted download can get a definite answer
/// without deleting and re-fetching first.
pub fn verify_parakeet_assets(asset_dir: &Path) -> Result<(), ModelError> {
    verify_manifest(asset_dir, &PARAKEET_ASSETS)
}

fn verify_manifest(asset_dir: &Path, manifest: &[ParakeetAsset]) -> Result<(), ModelError> {
    for asset in manifest {
        let path = asset_dir.join(asset.filename);
        if !path.is_file() {
            return Err(ModelError::Download(format!(
                "{} is not installed",
                asset.filename
            )));
        }
        let actual = sha256_file(&path)?;
        if !actual.eq_ignore_ascii_case(asset.sha256) {
            return Err(ModelError::Download(format!(
                "{} failed verification: expected {}, got {actual}",
                asset.filename, asset.sha256
            )));
        }
    }
    Ok(())
}

/// Install the pinned Parakeet assets. This is the only place in Slugtale that
/// fetches them, and it runs only when the user asks for it in Settings.
///
/// Reuses the [`ModelDownloader`] seam the Whisper install already goes through
/// so there is one HTTP implementation, one test double, and one place where a
/// proxy or a certificate problem can show up.
pub fn install_parakeet_assets(
    asset_dir: &Path,
    downloader: &dyn ModelDownloader,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<ParakeetAssetStatus, ModelError> {
    install_parakeet_manifest(asset_dir, &PARAKEET_ASSETS, downloader, on_progress)
}

/// Install an explicit manifest. The app always passes [`PARAKEET_ASSETS`];
/// taking the manifest as an argument is what makes the integrity boundary
/// testable with small deterministic fixtures instead of a 631 MiB download —
/// the same trick `local_model::ensure_default_model_with_sha256` uses for the
/// Whisper artefact.
///
/// Discipline, per file: download to a `.download` staging name, check the size,
/// check the digest, and only then rename into place. A staged file is deleted
/// on **every** failure path, so a failed or interrupted install can never leave
/// bytes that a later run would mistake for a finished download. Files that are
/// already installed at the right size are skipped, which makes a retry after a
/// dropped connection resume rather than start over.
pub fn install_parakeet_manifest(
    asset_dir: &Path,
    manifest: &[ParakeetAsset],
    downloader: &dyn ModelDownloader,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<ParakeetAssetStatus, ModelError> {
    std::fs::create_dir_all(asset_dir)?;

    let total = Some(parakeet_manifest_total_bytes(manifest));
    let mut completed = 0u64;
    on_progress(DownloadProgress {
        downloaded: completed,
        total,
    });

    for asset in manifest {
        if asset_file_is_installed(asset_dir, asset) {
            completed += asset.bytes;
            on_progress(DownloadProgress {
                downloaded: completed,
                total,
            });
            continue;
        }

        let staged_path = asset_dir.join(format!("{}.download", asset.filename));
        std::fs::remove_file(&staged_path).ok();

        // Progress is reported as one bar across the whole install, because the
        // user asked to install "Parakeet", not three files: per-file progress
        // that restarts at zero twice reads as a stall.
        let already_done = completed;
        downloader.download(&asset.download_url(), &staged_path, &mut |progress| {
            on_progress(DownloadProgress {
                downloaded: already_done + progress.downloaded,
                total,
            });
        })?;

        let downloaded_bytes = match staged_path.metadata() {
            Ok(metadata) => metadata.len(),
            Err(error) => {
                return Err(discard_staged_file(
                    &staged_path,
                    format!("could not read the downloaded {}: {error}", asset.filename),
                ));
            }
        };
        if downloaded_bytes != asset.bytes {
            return Err(discard_staged_file(
                &staged_path,
                format!(
                    "{} was incomplete: expected {} bytes, got {downloaded_bytes}",
                    asset.filename, asset.bytes
                ),
            ));
        }

        let actual_sha256 = match sha256_file(&staged_path) {
            Ok(digest) => digest,
            Err(error) => {
                return Err(discard_staged_file(
                    &staged_path,
                    format!("could not verify {}: {error}", asset.filename),
                ));
            }
        };
        if !actual_sha256.eq_ignore_ascii_case(asset.sha256) {
            return Err(discard_staged_file(
                &staged_path,
                format!(
                    "{} checksum mismatch: expected {}, got {actual_sha256}",
                    asset.filename, asset.sha256
                ),
            ));
        }

        std::fs::rename(&staged_path, asset_dir.join(asset.filename))?;
        completed += asset.bytes;
        on_progress(DownloadProgress {
            downloaded: completed,
            total,
        });
    }

    Ok(parakeet_asset_status_for_manifest(asset_dir, manifest))
}

/// Remove the installed assets and any staging leftovers, freeing the 631 MiB.
/// Missing files are not an error: the user asked for the model to be gone, and
/// it is.
pub fn delete_parakeet_assets(asset_dir: &Path) -> Result<ParakeetAssetStatus, ModelError> {
    for asset in PARAKEET_ASSETS {
        remove_file_if_present(&asset_dir.join(asset.filename))?;
        remove_file_if_present(&asset_dir.join(format!("{}.download", asset.filename)))?;
    }
    Ok(parakeet_asset_status(asset_dir))
}

fn remove_file_if_present(path: &Path) -> Result<(), ModelError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ModelError::Io(error)),
    }
}

/// Delete a staged download and report why it was rejected. Returning the
/// original reason even when the delete itself fails keeps the message the user
/// sees about the real problem, with the cleanup failure appended rather than
/// substituted.
fn discard_staged_file(path: &Path, message: String) -> ModelError {
    match std::fs::remove_file(path) {
        Ok(()) => ModelError::Download(message),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ModelError::Download(message),
        Err(error) => ModelError::Download(format!(
            "{message}; could not delete the invalid staged file: {error}"
        )),
    }
}

/// Hash a file in 64 KiB chunks rather than reading it into memory: the encoder
/// is 622 MiB and the reference machine has 8 GB.
fn sha256_file(path: &Path) -> Result<String, ModelError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

/// How many ONNX Runtime intra-op threads to use.
///
/// The same reasoning as the Whisper thread count (slugtale-jwy): oversubscribing
/// a conformer encoder makes it slower, not faster, and dictation runs while the
/// user's real work is also on the CPU. Cap at 8 because the encoder's gain
/// flattens well before that on the machines Slugtale targets, and leaving cores
/// free matters more than the last few percent. `available` of 0 means detection
/// failed; one thread always works.
#[cfg(any(test, feature = "local-parakeet-runtime"))]
fn parakeet_intra_threads(available: usize) -> usize {
    available.clamp(1, 8)
}

/// NVIDIA Parakeet TDT v2 0.6B behind the Transcription Engine boundary.
///
/// Construction is a directory path and one cheap filesystem probe. No ONNX
/// session is created and no 622 MiB encoder is read until
/// [`ParakeetProvider::warm_up`] or the first
/// [`TranscriptionProvider::transcribe`] runs, because a provider is built at
/// startup on every machine, including the ones where the user never turns
/// Parakeet on.
pub struct ParakeetProvider {
    asset_dir: PathBuf,
    /// Availability is answered from here, never from a fresh filesystem probe.
    /// The Second Opinion router asks on the dictation fast path, and three
    /// `stat` calls per dictation on a cold page cache is latency spent on a
    /// question whose answer only changes when the user installs or deletes the
    /// model — both of which call [`ParakeetProvider::refresh_availability`].
    availability: Mutex<EngineAvailability>,
    /// The loaded ONNX sessions. `Mutex` rather than `RwLock` because
    /// `parakeet-rs` transcription needs `&mut` (it advances the decoder), and
    /// the mutex doubles as the lifetime owner: shutdown takes the session so
    /// no decode can be in flight while the ONNX Runtime environment is torn
    /// down. Same discipline as `WhisperRuntimeCache::shutdown`.
    #[cfg(feature = "local-parakeet-runtime")]
    session: Mutex<Option<parakeet_rs::ParakeetTDT>>,
    #[cfg(feature = "local-parakeet-runtime")]
    shutting_down: std::sync::atomic::AtomicBool,
}

impl ParakeetProvider {
    /// Build a provider for assets installed under `asset_dir` — normally
    /// [`parakeet_asset_dir`] of Slugtale's models directory.
    pub fn new(asset_dir: PathBuf) -> Self {
        let availability = probe_availability(&asset_dir);
        Self {
            asset_dir,
            availability: Mutex::new(availability),
            #[cfg(feature = "local-parakeet-runtime")]
            session: Mutex::new(None),
            #[cfg(feature = "local-parakeet-runtime")]
            shutting_down: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn asset_dir(&self) -> &Path {
        &self.asset_dir
    }

    pub fn status(&self) -> ParakeetAssetStatus {
        parakeet_asset_status(&self.asset_dir)
    }

    /// Re-probe the filesystem and republish the cached answer. Settings calls
    /// this after an install or a delete; nothing on the dictation path does.
    pub fn refresh_availability(&self) -> EngineAvailability {
        let refreshed = probe_availability(&self.asset_dir);
        *lock(&self.availability) = refreshed.clone();
        refreshed
    }
}

/// The one place availability is decided, so Settings and the router cannot
/// disagree about why Parakeet is off.
fn probe_availability(asset_dir: &Path) -> EngineAvailability {
    if !cfg!(feature = "local-parakeet-runtime") {
        return EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt);
    }

    let status = parakeet_asset_status(asset_dir);
    if status.present {
        return EngineAvailability::Available;
    }

    // Deliberately says how many files rather than naming them: the count is
    // what the user needs, and a filename list grows unreadable in a settings
    // row. The exact names stay available on `ParakeetAssetStatus::missing`.
    EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing {
        detail: format!(
            "The Parakeet TDT v2 model has not been installed yet ({} of {} files missing).",
            status.missing.len(),
            PARAKEET_ASSETS.len()
        ),
    })
}

/// Take a lock without letting a panic elsewhere become a permanent failure.
///
/// A poisoned mutex here would make Parakeet unusable for the rest of the
/// session, and — because the router asks Parakeet on the same thread that
/// finishes a dictation — could turn one bad recording into a broken dictation
/// workflow. Nothing behind these mutexes has an invariant a panic could have
/// half-broken: one is a plain enum, the other an `Option` the caller is about
/// to replace. Recovering is strictly better than propagating.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Reject a recording Parakeet cannot decode, before anything expensive.
///
/// Runs first — ahead of the availability check — so that an ill-formed
/// recording produces the same, reproducible error on every build and every
/// machine, rather than being masked by "the model is not installed" on the
/// developer's laptop and only surfacing in production.
fn validate_captured_audio(audio: &CapturedAudio) -> Result<(), AsrError> {
    if audio.sample_rate_hz != 16_000 {
        return Err(AsrError::UnsupportedAudio(
            "Parakeet transcription expects 16 kHz mono f32 samples".to_string(),
        ));
    }
    if audio.samples.is_empty() {
        // The mel front-end windows the signal; an empty recording has no
        // frames to window and must not reach it.
        return Err(AsrError::UnsupportedAudio(
            "Parakeet transcription needs at least one audio sample".to_string(),
        ));
    }
    Ok(())
}

impl TranscriptionProvider for ParakeetProvider {
    fn engine(&self) -> TranscriptionEngine {
        PARAKEET_ENGINE
    }

    fn metadata(&self) -> EngineMetadata {
        EngineMetadata {
            engine: PARAKEET_ENGINE,
            model_id: PARAKEET_MODEL_ID,
            revision: PARAKEET_REVISION,
            approximate_bytes: Some(parakeet_total_bytes()),
            source_url: Some(PARAKEET_SOURCE_URL),
            license: PARAKEET_LICENSE,
            license_url: PARAKEET_LICENSE_URL,
            attribution: Some(PARAKEET_ATTRIBUTION),
            modifications: Some(PARAKEET_MODIFICATIONS),
            // Slugtale downloads and owns these files; no operating system
            // manages them, and Settings must not imply otherwise.
            system_managed: false,
            // ONNX Runtime, not Core ML, is what actually executes the graph, so
            // unlike the original Core ML design this engine is not Apple-only.
            supported_platforms: "macOS, Windows, and Linux",
        }
    }

    fn availability(&self) -> EngineAvailability {
        lock(&self.availability).clone()
    }

    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        validate_captured_audio(audio)?;
        self.transcribe_validated(audio)
    }
}

#[cfg(not(feature = "local-parakeet-runtime"))]
impl ParakeetProvider {
    /// Load the model ahead of the first dictation. Without the runtime feature
    /// there is nothing to load, and saying so is more useful than succeeding.
    pub fn warm_up(&self) -> Result<(), AsrError> {
        Err(runtime_not_built())
    }

    /// Release the loaded model. A no-op on this build; kept unconditional so
    /// the shutdown path does not need a `cfg`.
    pub fn shutdown(&self) {}

    /// Drop the loaded session without ending the provider, so a later
    /// selection of Parakeet can load it again. Nothing to drop on this build.
    pub fn unload(&self) {}

    fn transcribe_validated(
        &self,
        _audio: &CapturedAudio,
    ) -> Result<EngineTranscription, AsrError> {
        Err(runtime_not_built())
    }
}

#[cfg(not(feature = "local-parakeet-runtime"))]
fn runtime_not_built() -> AsrError {
    AsrError::EngineUnavailable {
        engine: PARAKEET_ENGINE,
        reason: EngineUnavailable::RuntimeNotBuilt,
    }
}

#[cfg(feature = "local-parakeet-runtime")]
impl ParakeetProvider {
    /// Load the ONNX sessions now so the first dictation does not pay for it.
    /// Reading and preparing a 622 MiB int8 encoder takes seconds; doing it
    /// while the user is waiting for their words would look like a hang.
    pub fn warm_up(&self) -> Result<(), AsrError> {
        self.with_session(|_| Ok(()))
    }

    /// Release the loaded model synchronously.
    ///
    /// Tauri's default `run` path ends in `process::exit`, which skips Rust
    /// destructors — the same reason `WhisperRuntimeCache::shutdown` exists
    /// (slugtale-p1u). ONNX Runtime holds a C++ environment and, on the Core ML
    /// path, Core ML/Metal globals; dropping the sessions here, under the lock
    /// that also serialises decoding, means no session is torn down while a
    /// decode is running and none is created afterwards.
    pub fn shutdown(&self) {
        use std::sync::atomic::Ordering;

        self.shutting_down.store(true, Ordering::Release);
        lock(&self.session).take();
    }

    /// Drop the loaded session without ending the provider, so a later
    /// selection of Parakeet can load it again. The same lock discipline as
    /// [`Self::shutdown`] makes this safe next to an in-flight decode: the
    /// session is only taken under the lock that serialises decoding.
    pub fn unload(&self) {
        lock(&self.session).take();
    }

    /// Run an operation against the cached session, loading it on first use.
    ///
    /// Holding the lock across the whole operation serialises decoding, which
    /// `parakeet-rs` requires anyway (`transcribe_samples` takes `&mut self`),
    /// and makes shutdown safe by construction.
    fn with_session<T>(
        &self,
        operation: impl FnOnce(&mut parakeet_rs::ParakeetTDT) -> Result<T, AsrError>,
    ) -> Result<T, AsrError> {
        use std::sync::atomic::Ordering;

        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AsrError::Runtime(
                "the Parakeet runtime is shutting down".to_string(),
            ));
        }

        let mut session = lock(&self.session);
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AsrError::Runtime(
                "the Parakeet runtime is shutting down".to_string(),
            ));
        }

        if session.is_none() {
            // Never fetch anything here. If the assets are absent this is a
            // recoverable "install it" answer, not a reason to reach for the
            // network — the network-denied test in slugtale-vjs.5 rests on this.
            let status = parakeet_asset_status(&self.asset_dir);
            if !status.present {
                let reason = EngineUnavailable::AssetsMissing {
                    detail: format!(
                        "The Parakeet TDT v2 model is not installed in {} ({} of {} files missing). Install it from Settings.",
                        self.asset_dir.display(),
                        status.missing.len(),
                        PARAKEET_ASSETS.len()
                    ),
                };
                *lock(&self.availability) = EngineAvailability::Unavailable(reason.clone());
                return Err(AsrError::EngineUnavailable {
                    engine: PARAKEET_ENGINE,
                    reason,
                });
            }

            *session = Some(self.load_session()?);
        }

        operation(session.as_mut().expect("session was just loaded"))
    }

    fn load_session(&self) -> Result<parakeet_rs::ParakeetTDT, AsrError> {
        // `mut` is used only on the Core ML build; the CPU build takes the
        // default provider and never reassigns.
        #[allow(unused_mut)]
        let mut config =
            parakeet_rs::ExecutionConfig::new().with_intra_threads(parakeet_intra_threads(
                std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(1),
            ));

        // The Core ML execution provider is opt-in and macOS-only. `parakeet-rs`
        // warns that Core ML can be *slower* than CPU for these graphs, because
        // their dynamic input shapes stop it planning for the Neural Engine and
        // it ends up claiming nodes it then runs on the CPU anyway. It is behind
        // its own Cargo feature for exactly that reason: benchmark slugtale-9dv
        // decides whether it is worth shipping. The compiled-model cache lives
        // beside the assets so the ~5 s conversion is paid once, not per launch.
        #[cfg(all(feature = "local-parakeet-runtime-coreml", target_os = "macos"))]
        {
            config = config
                .with_execution_provider(parakeet_rs::ExecutionProvider::CoreML)
                .with_coreml_cache_dir(self.asset_dir.join("coreml-cache"));
        }

        parakeet_rs::ParakeetTDT::from_pretrained(&self.asset_dir, Some(config)).map_err(|error| {
            // `parakeet-rs` errors describe files, ONNX graphs, and the
            // tokenizer. None of them can contain user content: this call has
            // not been given any audio yet.
            AsrError::Runtime(format!(
                "the Parakeet model in {} could not be loaded ({error}). Re-install it from Settings.",
                self.asset_dir.display()
            ))
        })
    }

    fn transcribe_validated(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        use parakeet_rs::Transcriber;

        let started = std::time::Instant::now();
        let result = self.with_session(|session| {
            // `parakeet-rs` takes ownership of the samples, so the clone is the
            // price of the borrowing signature every provider shares — the same
            // trade the Whisper adapter makes. A Second Opinion replays one
            // recording, not a stream, so this is one copy per escalation.
            session
                .transcribe_samples(
                    audio.samples.clone(),
                    audio.sample_rate_hz,
                    1,
                    Some(parakeet_rs::TimestampMode::Words),
                )
                .map_err(|error| {
                    // Deliberately does not interpolate `error` for a decode
                    // failure: a tokenizer or decoder message can quote the
                    // partial hypothesis, which is user content and must not
                    // reach an error string that gets logged.
                    let _ = error;
                    AsrError::Runtime(
                        "Parakeet could not decode this recording. Try dictating again."
                            .to_string(),
                    )
                })
        })?;

        Ok(EngineTranscription {
            engine: PARAKEET_ENGINE,
            transcription: FinalTranscription::plain(result.text.trim()),
            // TDT greedy decoding produces a single hypothesis. There is no
            // n-best list to expose, so the router selects between engines
            // rather than between Parakeet's own alternatives.
            alternatives: Vec::new(),
            // Parakeet's TDT decoder does emit per-token scores, but
            // `parakeet-rs` 0.3.6 does not expose them: its `TimedToken` carries
            // only `text`, `start`, and `end`, and the greedy decode in
            // `model_tdt` discards the joint logits after the argmax. There is
            // therefore no score to normalise, and inventing one — from token
            // count, from duration, from anything — would feed the Second
            // Opinion router a number that means nothing. Reporting nothing is
            // honest, and `EngineConfidence::unreported()` is explicitly not the
            // same as reporting low confidence: the router will escalate *from*
            // Parakeet on the transcript anomaly rules instead. Revisit if
            // `parakeet-rs` starts returning per-token log-probabilities.
            confidence: EngineConfidence::unreported(),
            latency: started.elapsed(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_carries_every_cc_by_obligation_settings_has_to_render() {
        // CC BY 4.0 requires the credit, the licence link, and a statement of
        // what changed. Settings renders these verbatim, so losing one here is a
        // licensing failure rather than a cosmetic one.
        let provider = ParakeetProvider::new(unique_test_dir("metadata"));
        let metadata = provider.metadata();

        assert_eq!(metadata.engine, TranscriptionEngine::Parakeet);
        assert_eq!(metadata.model_id, "nvidia/parakeet-tdt-0.6b-v2");
        assert_eq!(metadata.license, "CC BY 4.0");
        assert_eq!(
            metadata.license_url,
            "https://creativecommons.org/licenses/by/4.0/"
        );
        assert!(metadata
            .attribution
            .expect("CC BY 4.0 obliges an NVIDIA credit")
            .contains("NVIDIA"));
        let modifications = metadata
            .modifications
            .expect("CC BY 4.0 obliges a statement of changes");
        assert!(modifications.contains("ONNX"));
        assert!(modifications.contains("int8"));
        // Slugtale downloads these itself; claiming the OS manages them would
        // mislead the user about what is on their disk.
        assert!(!metadata.system_managed);
    }

    #[test]
    fn metadata_pins_a_commit_rather_than_a_branch() {
        // A floating `main` would let the bytes behind the pinned digests change
        // under an install the user already consented to.
        let provider = ParakeetProvider::new(unique_test_dir("revision"));
        let metadata = provider.metadata();

        assert_eq!(metadata.revision, PARAKEET_REVISION);
        let commit = PARAKEET_REVISION
            .split_once('@')
            .expect("the revision names a repository and a commit")
            .1;
        assert_eq!(commit.len(), 40, "a pinned revision is a full commit hash");
        assert!(commit.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(metadata
            .source_url
            .expect("Settings links to what it installs")
            .contains(commit));
    }

    #[test]
    fn metadata_reports_every_platform_onnx_runtime_covers() {
        let provider = ParakeetProvider::new(unique_test_dir("platforms"));

        // Unlike the original Core ML design, the ONNX artefacts are portable,
        // so the Linux and Windows ports inherit this engine.
        assert_eq!(
            provider.metadata().supported_platforms,
            "macOS, Windows, and Linux"
        );
    }

    #[test]
    fn metadata_size_matches_what_an_install_actually_downloads() {
        let provider = ParakeetProvider::new(unique_test_dir("size"));

        assert_eq!(
            provider.metadata().approximate_bytes,
            Some(parakeet_total_bytes())
        );
        // Roughly 631 MiB: the int8 export, not the 2.4 GiB fp32 one.
        assert!((600..700).contains(&(parakeet_total_bytes() / (1024 * 1024))));
    }

    #[test]
    fn every_pinned_asset_has_a_full_sha256_and_a_pinned_url() {
        for asset in PARAKEET_ASSETS {
            assert_eq!(
                asset.sha256.len(),
                64,
                "{} needs a full SHA-256 digest",
                asset.filename
            );
            assert!(
                asset
                    .sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{} digest must be lowercase hex",
                asset.filename
            );
            assert!(asset.bytes > 0);

            let url = asset.download_url();
            assert!(
                url.contains(PARAKEET_REVISION_COMMIT),
                "{url} must be pinned"
            );
            assert!(!url.contains("/main/"), "{url} must not float to a branch");
            assert!(url.ends_with(asset.filename));
        }
    }

    #[test]
    fn the_manifest_is_the_quantised_export_the_runtime_looks_for() {
        // `parakeet-rs` finds the encoder, the decoder-joint, and the vocabulary
        // by filename inside the model directory. Renaming any of these silently
        // turns a complete install into "no encoder model found".
        let names: Vec<&str> = PARAKEET_ASSETS.iter().map(|a| a.filename).collect();
        assert_eq!(
            names,
            vec![
                "vocab.txt",
                "decoder_joint-model.int8.onnx",
                "encoder-model.int8.onnx",
            ]
        );
    }

    #[test]
    fn status_lists_everything_an_install_would_have_to_fetch() {
        let asset_dir = unique_test_dir("status-missing");

        let status = parakeet_asset_status(&asset_dir);

        assert!(!status.present);
        assert_eq!(status.installed_bytes, 0);
        assert_eq!(status.missing.len(), PARAKEET_ASSETS.len());
    }

    #[test]
    fn a_truncated_file_counts_as_missing_rather_than_installed() {
        // A half-written encoder on disk must not read as a usable install; the
        // size check is the cheap guard that catches it without re-hashing
        // 631 MiB on the dictation path.
        let asset_dir = unique_test_dir("status-truncated");
        std::fs::create_dir_all(&asset_dir).unwrap();
        for asset in PARAKEET_ASSETS {
            std::fs::write(asset_dir.join(asset.filename), b"truncated").unwrap();
        }

        let status = parakeet_asset_status(&asset_dir);

        assert!(!status.present);
        assert_eq!(status.missing.len(), PARAKEET_ASSETS.len());
        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn install_verifies_each_file_then_renames_it_into_place() {
        let asset_dir = unique_test_dir("install-ok");
        let manifest = fixture_manifest();
        let downloader = FixtureDownloader::faithful();
        let mut updates = Vec::new();

        let status =
            install_parakeet_manifest(&asset_dir, &manifest, &downloader, &mut |progress| {
                updates.push(progress)
            })
            .expect("a faithful download installs");

        assert!(status.present);
        assert!(status.missing.is_empty());
        assert_eq!(
            status.installed_bytes,
            parakeet_manifest_total_bytes(&manifest)
        );
        assert_eq!(
            std::fs::read(asset_dir.join("vocab.txt")).unwrap(),
            b"parakeet vocabulary"
        );
        // Progress is one bar across the whole install, and it ends full.
        assert_eq!(
            updates.first().copied(),
            Some(DownloadProgress {
                downloaded: 0,
                total: Some(parakeet_manifest_total_bytes(&manifest)),
            })
        );
        assert_eq!(
            updates.last().copied(),
            Some(DownloadProgress {
                downloaded: parakeet_manifest_total_bytes(&manifest),
                total: Some(parakeet_manifest_total_bytes(&manifest)),
            })
        );
        // Nothing is left staged.
        assert!(!asset_dir.join("vocab.txt.download").exists());

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn install_rejects_a_checksum_mismatch_and_installs_nothing() {
        // Corrupt or substituted bytes must never become part of the model the
        // user's speech is decoded by.
        let asset_dir = unique_test_dir("install-bad-digest");
        let manifest = fixture_manifest();
        let downloader = FixtureDownloader::tampering_with("vocab.txt");

        let error = install_parakeet_manifest(&asset_dir, &manifest, &downloader, &mut |_| {})
            .expect_err("a digest mismatch fails the install");

        assert!(error.to_string().contains("checksum mismatch"));
        assert!(!asset_dir.join("vocab.txt").exists());
        assert!(
            !asset_dir.join("vocab.txt.download").exists(),
            "the staged file must be deleted so a retry cannot adopt it"
        );
        assert!(!parakeet_asset_status_for_manifest(&asset_dir, &manifest).present);

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn install_rejects_a_truncated_download_before_hashing_it() {
        let asset_dir = unique_test_dir("install-short");
        let manifest = fixture_manifest();
        let downloader = FixtureDownloader::truncating("decoder_joint-model.int8.onnx");

        let error = install_parakeet_manifest(&asset_dir, &manifest, &downloader, &mut |_| {})
            .expect_err("a short download fails the install");

        assert!(error.to_string().contains("was incomplete"));
        assert!(!asset_dir.join("decoder_joint-model.int8.onnx").exists());
        assert!(!asset_dir
            .join("decoder_joint-model.int8.onnx.download")
            .exists());
        // The file that did succeed stays; a retry resumes from there.
        assert!(asset_dir.join("vocab.txt").exists());

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn install_resumes_rather_than_re_downloading_finished_files() {
        // 631 MiB over a flaky connection needs more than one attempt; the
        // second attempt must not start from zero.
        let asset_dir = unique_test_dir("install-resume");
        let manifest = fixture_manifest();

        let first = FixtureDownloader::truncating("encoder-model.int8.onnx");
        install_parakeet_manifest(&asset_dir, &manifest, &first, &mut |_| {})
            .expect_err("the first attempt fails on the encoder");

        let second = FixtureDownloader::faithful();
        let status = install_parakeet_manifest(&asset_dir, &manifest, &second, &mut |_| {})
            .expect("the retry completes the install");

        assert!(status.present);
        assert_eq!(
            second.requested_filenames(),
            vec!["encoder-model.int8.onnx"],
            "already-installed files must not be fetched again"
        );

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn verification_reads_the_digests_and_reports_the_offending_file() {
        let asset_dir = unique_test_dir("verify");
        let manifest = fixture_manifest();
        install_parakeet_manifest(
            &asset_dir,
            &manifest,
            &FixtureDownloader::faithful(),
            &mut |_| {},
        )
        .unwrap();

        assert!(verify_manifest(&asset_dir, &manifest).is_ok());

        // Same length, different bytes: only the digest can catch this.
        std::fs::write(asset_dir.join("vocab.txt"), b"parakeet vocabulaRy").unwrap();
        let error = verify_manifest(&asset_dir, &manifest).unwrap_err();
        assert!(error.to_string().contains("vocab.txt"));
        assert!(error.to_string().contains("failed verification"));

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn deleting_the_model_frees_the_disk_and_clears_the_staging_area() {
        let asset_dir = unique_test_dir("delete");
        std::fs::create_dir_all(&asset_dir).unwrap();
        for asset in PARAKEET_ASSETS {
            std::fs::write(asset_dir.join(asset.filename), b"installed").unwrap();
            std::fs::write(
                asset_dir.join(format!("{}.download", asset.filename)),
                b"staged",
            )
            .unwrap();
        }

        let status = delete_parakeet_assets(&asset_dir).expect("delete succeeds");

        assert!(!status.present);
        assert_eq!(status.installed_bytes, 0);
        for asset in PARAKEET_ASSETS {
            assert!(!asset_dir.join(asset.filename).exists());
            assert!(!asset_dir
                .join(format!("{}.download", asset.filename))
                .exists());
        }
        // Deleting a model that is already gone is what the user asked for.
        assert!(delete_parakeet_assets(&asset_dir).is_ok());

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn availability_is_answered_from_cache_not_from_the_filesystem() {
        // The Second Opinion router asks this on the dictation fast path. If it
        // hit the disk, every dictation would pay for three `stat` calls to
        // answer a question that only changes when the user installs or deletes.
        let asset_dir = unique_test_dir("availability-cache");
        std::fs::create_dir_all(&asset_dir).unwrap();
        let provider = ParakeetProvider::new(asset_dir.clone());
        let at_construction = provider.availability();

        // Make the filesystem disagree with the cache in both directions.
        for asset in PARAKEET_ASSETS {
            std::fs::write(asset_dir.join(asset.filename), b"x").unwrap();
        }
        assert_eq!(provider.availability(), at_construction);

        std::fs::remove_dir_all(&asset_dir).ok();
        assert_eq!(provider.availability(), at_construction);
    }

    #[test]
    fn refreshing_availability_republishes_the_cached_answer() {
        let asset_dir = unique_test_dir("availability-refresh");
        let provider = ParakeetProvider::new(asset_dir.clone());

        let refreshed = provider.refresh_availability();

        assert_eq!(refreshed, provider.availability());
        assert!(!refreshed.is_available());
    }

    #[cfg(not(feature = "local-parakeet-runtime"))]
    #[test]
    fn a_build_without_the_runtime_says_so_instead_of_offering_an_install() {
        // Settings turns `AssetsMissing` into a download button. Offering one on
        // a build that could not use the model would be a dead end.
        let provider = ParakeetProvider::new(unique_test_dir("runtime-not-built"));

        assert_eq!(
            provider.availability(),
            EngineAvailability::Unavailable(EngineUnavailable::RuntimeNotBuilt)
        );
        assert!(!EngineUnavailable::RuntimeNotBuilt.is_user_resolvable());
    }

    #[cfg(feature = "local-parakeet-runtime")]
    #[test]
    fn a_runtime_build_without_assets_offers_an_install() {
        let provider = ParakeetProvider::new(unique_test_dir("assets-missing"));

        match provider.availability() {
            EngineAvailability::Unavailable(reason) => {
                assert!(reason.is_user_resolvable());
                assert!(matches!(reason, EngineUnavailable::AssetsMissing { .. }));
            }
            EngineAvailability::Available => panic!("no assets are installed"),
        }
    }

    #[test]
    fn transcription_rejects_audio_that_is_not_16_khz_mono() {
        // Audio Capture hands the workflow 16 kHz mono f32; anything else is a
        // wiring mistake that must fail loudly rather than be resampled by
        // accident inside the ONNX front-end.
        let provider = ParakeetProvider::new(unique_test_dir("wrong-rate"));

        let error = provider
            .transcribe(&CapturedAudio {
                sample_rate_hz: 44_100,
                samples: vec![0.0; 44_100],
            })
            .unwrap_err();

        assert_eq!(
            error,
            AsrError::UnsupportedAudio(
                "Parakeet transcription expects 16 kHz mono f32 samples".to_string()
            )
        );
    }

    #[test]
    fn transcription_rejects_an_empty_recording() {
        let provider = ParakeetProvider::new(unique_test_dir("empty-audio"));

        let error = provider
            .transcribe(&CapturedAudio::mono_16khz(Vec::new()))
            .unwrap_err();

        assert_eq!(
            error,
            AsrError::UnsupportedAudio(
                "Parakeet transcription needs at least one audio sample".to_string()
            )
        );
    }

    #[test]
    fn a_missing_model_is_an_actionable_error_and_never_a_panic() {
        // Whisper has to stay usable when Parakeet is not installed, so this
        // path returns rather than unwinding, and the message tells the user
        // what to do about it.
        let provider = ParakeetProvider::new(unique_test_dir("missing-model"));

        let error = provider
            .transcribe(&CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .unwrap_err();

        match &error {
            AsrError::EngineUnavailable { engine, reason } => {
                assert_eq!(*engine, TranscriptionEngine::Parakeet);
                if cfg!(feature = "local-parakeet-runtime") {
                    assert!(matches!(reason, EngineUnavailable::AssetsMissing { .. }));
                    assert!(error.to_string().contains("Settings"));
                } else {
                    assert_eq!(*reason, EngineUnavailable::RuntimeNotBuilt);
                }
            }
            other => panic!("expected an unavailable engine, got {other:?}"),
        }

        // The provider survives it: a second attempt reports the same thing
        // rather than a poisoned lock.
        assert!(provider
            .transcribe(&CapturedAudio::mono_16khz(vec![0.0; 16_000]))
            .is_err());
        assert!(!provider.availability().is_available());
    }

    #[test]
    fn shutdown_is_idempotent_and_leaves_the_provider_answerable() {
        // Shutdown runs on the exit path, possibly twice, and Settings may still
        // ask for metadata while the window closes.
        let provider = ParakeetProvider::new(unique_test_dir("shutdown"));

        provider.shutdown();
        provider.shutdown();

        assert_eq!(provider.engine(), TranscriptionEngine::Parakeet);
        assert!(!provider.availability().is_available());
    }

    #[cfg(feature = "local-parakeet-runtime")]
    #[test]
    fn nothing_loads_after_shutdown() {
        let provider = ParakeetProvider::new(unique_test_dir("shutdown-blocks-load"));
        provider.shutdown();

        assert_eq!(
            provider.warm_up().unwrap_err(),
            AsrError::Runtime("the Parakeet runtime is shutting down".to_string())
        );
    }

    #[test]
    fn onnx_threads_stay_within_what_the_machine_offers() {
        // Oversubscribing a conformer encoder makes it slower, and dictation
        // runs alongside the user's real work.
        assert_eq!(parakeet_intra_threads(4), 4);
        assert_eq!(parakeet_intra_threads(32), 8);
        assert_eq!(parakeet_intra_threads(0), 1);
    }

    #[test]
    fn the_engine_never_prints_anything() {
        // Audio, transcripts, and confidence derived from them must not reach
        // stdout, stderr, or the Local Diagnostic Log (ADR-0019). The cheapest
        // durable guard is to forbid the macros outright: there is no legitimate
        // reason for this module to print, so a debugging leftover fails here.
        let source = include_str!("parakeet.rs");
        // The names are assembled at runtime so this test's own list does not
        // put the forbidden text into the file it is scanning.
        for stem in ["print", "eprint", "dbg"] {
            for macro_name in [format!("{stem}!"), format!("{stem}ln!")] {
                assert!(
                    !source.contains(&macro_name),
                    "{macro_name} must not appear in the Parakeet engine"
                );
            }
        }
    }

    /// A miniature stand-in for the pinned manifest: same shape, same
    /// verification path, bytes small enough to live in the test binary.
    fn fixture_manifest() -> Vec<ParakeetAsset> {
        vec![
            ParakeetAsset {
                filename: "vocab.txt",
                bytes: 19,
                // sha256("parakeet vocabulary")
                sha256: "5e4bb40b49c813426a3b451c02aafff78be5ff99eea7fbbb97841bbd48d74521",
            },
            ParakeetAsset {
                filename: "decoder_joint-model.int8.onnx",
                bytes: 18,
                // sha256("onnx-decoder-joint")
                sha256: "1f678cfed5a23bded7685c23d1e2f9e11b6f2a6777a82dc11b9456d5da52076b",
            },
            ParakeetAsset {
                filename: "encoder-model.int8.onnx",
                bytes: 18,
                // sha256("onnx-encoder-graph")
                sha256: "73a7f3fab35ace7bbe4855011335f15ee810a02ad3bdc77a1b3cab503cac5cfd",
            },
        ]
    }

    struct FixtureDownloader {
        /// Filename the double should mistreat, and how.
        sabotage: Option<(&'static str, Sabotage)>,
        requested: std::sync::Mutex<Vec<String>>,
    }

    #[derive(Clone, Copy)]
    enum Sabotage {
        /// Right length, wrong bytes — only the digest catches it.
        Tamper,
        /// Short read, as a dropped connection produces.
        Truncate,
    }

    impl FixtureDownloader {
        fn faithful() -> Self {
            Self {
                sabotage: None,
                requested: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn tampering_with(filename: &'static str) -> Self {
            Self {
                sabotage: Some((filename, Sabotage::Tamper)),
                requested: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn truncating(filename: &'static str) -> Self {
            Self {
                sabotage: Some((filename, Sabotage::Truncate)),
                requested: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn requested_filenames(&self) -> Vec<String> {
            self.requested.lock().unwrap().clone()
        }

        fn body_for(filename: &str) -> &'static [u8] {
            match filename {
                "vocab.txt" => b"parakeet vocabulary",
                "decoder_joint-model.int8.onnx" => b"onnx-decoder-joint",
                _ => b"onnx-encoder-graph",
            }
        }
    }

    impl ModelDownloader for FixtureDownloader {
        fn download(
            &self,
            url: &str,
            destination: &Path,
            on_progress: &mut dyn FnMut(DownloadProgress),
        ) -> Result<(), ModelError> {
            let filename = url.rsplit('/').next().unwrap_or_default().to_string();
            self.requested.lock().unwrap().push(filename.clone());

            let mut body = Self::body_for(&filename).to_vec();
            match self.sabotage {
                Some((target, Sabotage::Tamper)) if target == filename => {
                    // Same length so only the digest can reject it.
                    let last = body.len() - 1;
                    body[last] = b'!';
                }
                Some((target, Sabotage::Truncate)) if target == filename => {
                    body.truncate(body.len() / 2);
                }
                _ => {}
            }

            on_progress(DownloadProgress {
                downloaded: 0,
                total: Some(body.len() as u64),
            });
            std::fs::write(destination, &body)?;
            on_progress(DownloadProgress {
                downloaded: body.len() as u64,
                total: Some(body.len() as u64),
            });
            Ok(())
        }
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-parakeet-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
