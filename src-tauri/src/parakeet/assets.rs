use crate::{DownloadProgress, ModelDownloader, ModelError};
use std::path::{Path, PathBuf};

/// Where one TDT model's files come from and where they go: a pinned upstream
/// repository commit, the directory they are installed under, and the files
/// themselves. Parakeet TDT v2 and Phonon-2 are both TDT models `parakeet-rs`
/// loads the same way, so they differ only in this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TdtModelFiles {
    /// The Hugging Face repository the files are fetched from.
    pub repo: &'static str,
    /// The commit the files are fetched at. A commit hash and never a branch:
    /// a floating `main` would let the artefact behind the pinned digests change
    /// under us, which both breaks the digests and quietly changes what the
    /// user installed after they consented to a specific model.
    pub commit: &'static str,
    /// The directory name the files are installed under, inside Slugtale's
    /// models directory. A subdirectory rather than loose files because
    /// `parakeet-rs` loads a model *directory* and picks the encoder, decoder,
    /// and vocabulary out of it by filename; the Whisper `.bin` sitting
    /// alongside them would be noise.
    pub dir_name: &'static str,
    pub assets: &'static [ParakeetAsset],
}

impl TdtModelFiles {
    /// The pinned download URL for one of this model's files. Built from the
    /// commit, never from a branch, so re-running an install a year from now
    /// fetches the same bytes.
    pub fn download_url(&self, asset: &ParakeetAsset) -> String {
        format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            self.repo, self.commit, asset.filename
        )
    }

    /// How much disk a complete install takes, for the Settings copy and for
    /// the aggregate download progress bar.
    pub fn total_bytes(&self) -> u64 {
        self.assets.iter().map(|asset| asset.bytes).sum()
    }

    /// Where these files live for a given models directory.
    pub fn asset_dir(&self, model_dir: &Path) -> PathBuf {
        model_dir.join(self.dir_name)
    }
}

/// Parakeet TDT v2 at a pinned revision of istupakov's ONNX export.
///
/// These are the **int8** artefacts, not the full-precision ones. The fp32
/// encoder is 2.4 GiB of external weights; on the 8 GB reference machine that is
/// a download most users would abandon and a resident memory cost that would
/// crowd out Whisper. The int8 export is 631 MiB in total and is what
/// `parakeet-rs` is exercised against upstream. That choice is a *modification*
/// under CC BY 4.0, which is why the engine metadata states it.
///
/// The order matters a little: the vocabulary is tiny and comes first, so a
/// mistyped asset directory or a read-only disk fails in a second rather than
/// after a 600 MiB download.
pub const PARAKEET_FILES: TdtModelFiles = TdtModelFiles {
    repo: "istupakov/parakeet-tdt-0.6b-v2-onnx",
    commit: "0bbb45a3365852604aef28b538a8f066f4ccaa85",
    dir_name: "parakeet-tdt-0.6b-v2",
    assets: &[
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
    ],
};

/// Phonon-2 at a pinned revision of Tiyuvta's ONNX export.
///
/// The encoder is the **exact4x2** artefact: Phonon-2's five-level weights
/// written bit for bit as two ternary planes in 4-bit `MatMulNBits`. The export
/// reports it matching Fermion's fp32 runtime token for token at about half the
/// fp32 encoder's memory, where the int8 encoder is smaller still but lossy.
/// `parakeet-rs` does not list `encoder-model.exact4x2.onnx` among its encoder
/// names, but falls back to the one `encoder*.onnx` in the directory, which is
/// why no other encoder may be installed beside it.
///
/// The decoder-joint is fp32 in every variant the export ships, so the plain
/// name is the one `parakeet-rs` looks for first.
pub const PHONON_FILES: TdtModelFiles = TdtModelFiles {
    repo: "tiyuvta/Phonon-2-ONNX",
    commit: "12c9688bbc4fc52d23c1a66ca873fd3ac6ed4408",
    dir_name: "phonon-2",
    assets: &[
        ParakeetAsset {
            filename: "vocab.txt",
            bytes: 93_939,
            sha256: "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        },
        ParakeetAsset {
            filename: "decoder_joint-model.onnx",
            bytes: 72_518_934,
            sha256: "420125e0e13596692320c35ef648eee9bf4583718c7896c8732ebf6f50b9ca0d",
        },
        ParakeetAsset {
            filename: "encoder-model.exact4x2.onnx",
            bytes: 662_190_977,
            sha256: "abfdefaa1c74d6d3ca367a7ed358732a6140fb26a312b650ee57e46f1a9849ec",
        },
    ],
};

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
/// that a file went missing or was truncated afterwards.
pub fn parakeet_asset_status(asset_dir: &Path, files: &TdtModelFiles) -> ParakeetAssetStatus {
    let mut installed_bytes = 0;
    let mut missing = Vec::new();

    for asset in files.assets {
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

#[cfg(test)]
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

/// Install one TDT model's pinned files. This is the only place in Slugtale that
/// fetches them, and it runs only when the user asks for it in Settings.
///
/// Reuses the [`ModelDownloader`] seam the Whisper install already goes through
/// so there is one HTTP implementation, one test double, and one place where a
/// proxy or a certificate problem can show up. The app passes
/// [`PARAKEET_FILES`] or [`PHONON_FILES`]; taking the file set as an argument is
/// also what makes the integrity boundary testable with small deterministic
/// fixtures instead of a 631 MiB download — the same trick
/// `local_model::ensure_default_model_with_sha256` uses for the Whisper
/// artefact.
///
/// Discipline, per file: download to a `.download` staging name, check the size,
/// check the digest, and only then rename into place. A staged file is deleted
/// on **every** failure path, so a failed or interrupted install can never leave
/// bytes that a later run would mistake for a finished download. Files that are
/// already installed at the right size are skipped, which makes a retry after a
/// dropped connection resume rather than start over.
pub fn install_parakeet_assets(
    asset_dir: &Path,
    files: &TdtModelFiles,
    downloader: &dyn ModelDownloader,
    on_progress: &mut dyn FnMut(DownloadProgress),
) -> Result<ParakeetAssetStatus, ModelError> {
    std::fs::create_dir_all(asset_dir)?;

    let total = Some(files.total_bytes());
    let mut completed = 0u64;
    on_progress(DownloadProgress {
        downloaded: completed,
        total,
    });

    for asset in files.assets {
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
        downloader.download(&files.download_url(asset), &staged_path, &mut |progress| {
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

    Ok(parakeet_asset_status(asset_dir, files))
}

/// Remove the installed assets and any staging leftovers, freeing the disk.
/// Missing files are not an error: the user asked for the model to be gone, and
/// it is.
pub fn delete_parakeet_assets(
    asset_dir: &Path,
    files: &TdtModelFiles,
) -> Result<ParakeetAssetStatus, ModelError> {
    for asset in files.assets {
        remove_file_if_present(&asset_dir.join(asset.filename))?;
        remove_file_if_present(&asset_dir.join(format!("{}.download", asset.filename)))?;
    }
    Ok(parakeet_asset_status(asset_dir, files))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pinned_asset_has_a_full_sha256_and_a_pinned_url() {
        for (files, asset) in [PARAKEET_FILES, PHONON_FILES]
            .iter()
            .flat_map(|files| files.assets.iter().map(move |asset| (files, asset)))
        {
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

            assert_eq!(
                files.commit.len(),
                40,
                "a pinned revision is a full commit hash"
            );
            let url = files.download_url(asset);
            assert!(url.contains(files.commit), "{url} must be pinned");
            assert!(!url.contains("/main/"), "{url} must not float to a branch");
            assert!(url.ends_with(asset.filename));
        }
    }

    #[test]
    fn the_manifest_is_the_quantised_export_the_runtime_looks_for() {
        // `parakeet-rs` finds the encoder, the decoder-joint, and the vocabulary
        // by filename inside the model directory. Renaming any of these silently
        // turns a complete install into "no encoder model found".
        let names: Vec<&str> = PARAKEET_FILES.assets.iter().map(|a| a.filename).collect();
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
    fn the_phonon_files_are_the_exact_encoder_and_the_one_decoder_the_runtime_finds() {
        // `parakeet-rs` reaches the exact4x2 encoder only through its
        // "any encoder*.onnx" fallback, so exactly one encoder may be listed,
        // and the decoder has to carry the first name it looks for.
        let names: Vec<&str> = PHONON_FILES.assets.iter().map(|a| a.filename).collect();
        assert_eq!(
            names,
            vec![
                "vocab.txt",
                "decoder_joint-model.onnx",
                "encoder-model.exact4x2.onnx",
            ]
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| name.starts_with("encoder"))
                .count(),
            1
        );
    }

    #[test]
    fn each_model_installs_into_its_own_directory() {
        // Two TDT models in one directory would hand `parakeet-rs` two encoders
        // and two vocabularies to choose between.
        let model_dir = Path::new("models");
        assert_ne!(
            PARAKEET_FILES.asset_dir(model_dir),
            PHONON_FILES.asset_dir(model_dir)
        );
    }

    #[test]
    fn status_lists_everything_an_install_would_have_to_fetch() {
        let asset_dir = unique_test_dir("status-missing");

        let status = parakeet_asset_status(&asset_dir, &PARAKEET_FILES);

        assert!(!status.present);
        assert_eq!(status.installed_bytes, 0);
        assert_eq!(status.missing.len(), PARAKEET_FILES.assets.len());
    }

    #[test]
    fn a_truncated_file_counts_as_missing_rather_than_installed() {
        // A half-written encoder on disk must not read as a usable install; the
        // size check is the cheap guard that catches it without re-hashing
        // 631 MiB on the dictation path.
        let asset_dir = unique_test_dir("status-truncated");
        std::fs::create_dir_all(&asset_dir).unwrap();
        for asset in PARAKEET_FILES.assets {
            std::fs::write(asset_dir.join(asset.filename), b"truncated").unwrap();
        }

        let status = parakeet_asset_status(&asset_dir, &PARAKEET_FILES);

        assert!(!status.present);
        assert_eq!(status.missing.len(), PARAKEET_FILES.assets.len());
        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn install_verifies_each_file_then_renames_it_into_place() {
        let asset_dir = unique_test_dir("install-ok");
        let manifest = FIXTURE_FILES;
        let downloader = FixtureDownloader::faithful();
        let mut updates = Vec::new();

        let status = install_parakeet_assets(&asset_dir, &manifest, &downloader, &mut |progress| {
            updates.push(progress)
        })
        .expect("a faithful download installs");

        assert!(status.present);
        assert!(status.missing.is_empty());
        assert_eq!(status.installed_bytes, manifest.total_bytes());
        assert_eq!(
            std::fs::read(asset_dir.join("vocab.txt")).unwrap(),
            b"parakeet vocabulary"
        );
        // Progress is one bar across the whole install, and it ends full.
        assert_eq!(
            updates.first().copied(),
            Some(DownloadProgress {
                downloaded: 0,
                total: Some(manifest.total_bytes()),
            })
        );
        assert_eq!(
            updates.last().copied(),
            Some(DownloadProgress {
                downloaded: manifest.total_bytes(),
                total: Some(manifest.total_bytes()),
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
        let manifest = FIXTURE_FILES;
        let downloader = FixtureDownloader::tampering_with("vocab.txt");

        let error = install_parakeet_assets(&asset_dir, &manifest, &downloader, &mut |_| {})
            .expect_err("a digest mismatch fails the install");

        assert!(error.to_string().contains("checksum mismatch"));
        assert!(!asset_dir.join("vocab.txt").exists());
        assert!(
            !asset_dir.join("vocab.txt.download").exists(),
            "the staged file must be deleted so a retry cannot adopt it"
        );
        assert!(!parakeet_asset_status(&asset_dir, &manifest).present);

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn install_rejects_a_truncated_download_before_hashing_it() {
        let asset_dir = unique_test_dir("install-short");
        let manifest = FIXTURE_FILES;
        let downloader = FixtureDownloader::truncating("decoder_joint-model.int8.onnx");

        let error = install_parakeet_assets(&asset_dir, &manifest, &downloader, &mut |_| {})
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
        let manifest = FIXTURE_FILES;

        let first = FixtureDownloader::truncating("encoder-model.int8.onnx");
        install_parakeet_assets(&asset_dir, &manifest, &first, &mut |_| {})
            .expect_err("the first attempt fails on the encoder");

        let second = FixtureDownloader::faithful();
        let status = install_parakeet_assets(&asset_dir, &manifest, &second, &mut |_| {})
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
        let manifest = FIXTURE_FILES;
        install_parakeet_assets(
            &asset_dir,
            &manifest,
            &FixtureDownloader::faithful(),
            &mut |_| {},
        )
        .unwrap();

        assert!(verify_manifest(&asset_dir, manifest.assets).is_ok());

        // Same length, different bytes: only the digest can catch this.
        std::fs::write(asset_dir.join("vocab.txt"), b"parakeet vocabulaRy").unwrap();
        let error = verify_manifest(&asset_dir, manifest.assets).unwrap_err();
        assert!(error.to_string().contains("vocab.txt"));
        assert!(error.to_string().contains("failed verification"));

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    #[test]
    fn deleting_the_model_frees_the_disk_and_clears_the_staging_area() {
        let asset_dir = unique_test_dir("delete");
        std::fs::create_dir_all(&asset_dir).unwrap();
        for asset in PARAKEET_FILES.assets {
            std::fs::write(asset_dir.join(asset.filename), b"installed").unwrap();
            std::fs::write(
                asset_dir.join(format!("{}.download", asset.filename)),
                b"staged",
            )
            .unwrap();
        }

        let status = delete_parakeet_assets(&asset_dir, &PARAKEET_FILES).expect("delete succeeds");

        assert!(!status.present);
        assert_eq!(status.installed_bytes, 0);
        for asset in PARAKEET_FILES.assets {
            assert!(!asset_dir.join(asset.filename).exists());
            assert!(!asset_dir
                .join(format!("{}.download", asset.filename))
                .exists());
        }
        // Deleting a model that is already gone is what the user asked for.
        assert!(delete_parakeet_assets(&asset_dir, &PARAKEET_FILES).is_ok());

        std::fs::remove_dir_all(&asset_dir).ok();
    }

    /// A miniature stand-in for the pinned manifest: same shape, same
    /// verification path, bytes small enough to live in the test binary.
    const FIXTURE_FILES: TdtModelFiles = TdtModelFiles {
        repo: "slugtale/fixture",
        commit: "0000000000000000000000000000000000000000",
        dir_name: "fixture",
        assets: &[
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
        ],
    };

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
