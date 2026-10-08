use crate::{
    AsrError, AssetInstall, CapturedAudio, DownloadProgress, EngineAssetLifecycle, EngineAssets,
    EngineAvailability, EngineMetadata, EngineTranscriber, EngineTranscription, EngineUnavailable,
    FinalTranscription, HttpModelDownloader, ModelDownloader, TranscriptionEngine,
};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex, MutexGuard,
};
use std::time::{Duration, Instant};

const WORKER: &str = include_str!("../../phonon/worker.py");
const SETUP: &str = include_str!("../../phonon/setup.py");
const REQUIREMENTS: &str = include_str!("../../phonon/requirements.lock");
const REVISION: &str = "ca1bef26bcd8ef4a7e16d0636d8a77bb25e298ee";
const SOURCE: &str =
    "https://huggingface.co/FermionResearch/Phonon-2/tree/ca1bef26bcd8ef4a7e16d0636d8a77bb25e298ee";
const SANDBOX: &str = "(version 1)(allow default)(deny network*)";
const START_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_SAMPLES: usize = 16_000 * 60 * 30;
const MAX_RESPONSE: u64 = 1_048_576;
const PYTHON_URL: &str = "https://github.com/astral-sh/python-build-standalone/releases/download/20261001/cpython-3.12.15%2B20261001-aarch64-apple-darwin-install_only.tar.gz";

pub(crate) fn supported_os() -> bool {
    Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|version| version.trim().split('.').next()?.parse::<u32>().ok())
        .is_some_and(|major| major >= 14)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn runtime_error() -> AsrError {
    AsrError::Runtime("Phonon-2 MLX stopped or took too long. Try again. If this repeats, reinstall it from Settings.".into())
}

fn install_stamp() -> String {
    let mut hash = Sha256::new();
    hash.update(REQUIREMENTS);
    hash.update(SETUP);
    hash.update(WORKER);
    hash.update(REVISION);
    format!("{:x}", hash.finalize())
}

pub(crate) struct MlxProvider {
    root: PathBuf,
    /// Serialises install and removal. It is held across downloads and setup,
    /// so `shutdown` and `unload` never take it.
    install_gate: Mutex<()>,
    /// Held only to swap, take, or check the worker, never across a download
    /// or setup step. `changing` is written under this lock as well.
    worker: Mutex<Option<Worker>>,
    /// Set while assets are being installed or removed, so a decode cannot
    /// start a worker from partial files.
    changing: AtomicBool,
    availability: Mutex<EngineAvailability>,
    /// Owns the running install step. Shutdown closes it under its own lock.
    stop: Stop,
}

impl MlxProvider {
    pub(crate) fn new(root: PathBuf) -> Self {
        let availability = probe(&root);
        Self {
            root,
            install_gate: Mutex::new(()),
            worker: Mutex::new(None),
            changing: AtomicBool::new(false),
            availability: Mutex::new(availability),
            stop: Stop::default(),
        }
    }

    pub fn unload(&self) {
        lock(&self.worker).take();
    }

    pub fn shutdown(&self) {
        self.stop.close();
        self.unload();
    }

    fn begin_change(&self) {
        let mut slot = lock(&self.worker);
        slot.take();
        self.changing.store(true, Ordering::Release);
    }

    fn finish_change(&self) {
        let availability = probe(&self.root);
        let _slot = lock(&self.worker);
        self.changing.store(false, Ordering::Release);
        *lock(&self.availability) = availability;
    }

    fn with_worker<T>(
        &self,
        operation: impl FnOnce(&mut Worker) -> Result<T, AsrError>,
    ) -> Result<T, AsrError> {
        let mut slot = lock(&self.worker);
        if self.stop.is_closed() {
            return Err(runtime_error());
        }
        if self.changing.load(Ordering::Acquire) {
            return Err(AsrError::EngineUnavailable {
                engine: TranscriptionEngine::Phonon,
                reason: EngineUnavailable::AssetsMissing {
                    detail: "Phonon-2 MLX is being installed or removed. Try again when Settings finishes.".into(),
                },
            });
        }
        if slot.is_none() {
            let availability = probe(&self.root);
            *lock(&self.availability) = availability.clone();
            if let EngineAvailability::Unavailable(reason) = availability {
                return Err(AsrError::EngineUnavailable {
                    engine: TranscriptionEngine::Phonon,
                    reason,
                });
            }
            *slot = Some(Worker::start(&self.root)?);
        }
        let result = operation(slot.as_mut().expect("worker was loaded"));
        if result.is_err() {
            slot.take();
        } // Kill/reap; next request can restart cleanly.
        result
    }

    fn install_with(
        &self,
        downloader: &dyn ModelDownloader,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        let _gate = lock(&self.install_gate);
        cancelled(&self.stop)?;
        self.begin_change();
        let result = install(&self.root, downloader, &self.stop, on_progress);
        self.finish_change();
        result?;
        Ok(AssetInstall { warm_up: true })
    }
}

fn probe(root: &Path) -> EngineAvailability {
    let complete = std::fs::read_to_string(root.join("installed"))
        .is_ok_and(|stamp| stamp == install_stamp())
        && root.join("python/bin/python3").is_file()
        && root
            .join("site/fermion/_speech/engine_phonon2.py")
            .is_file()
        && root.join("model/config.json").is_file()
        && root
            .join("model/model.fermion")
            .metadata()
            .is_ok_and(|m| m.len() == 177_438_361);
    if complete {
        EngineAvailability::Available
    } else {
        EngineAvailability::Unavailable(EngineUnavailable::AssetsMissing {
            detail: "Install Phonon-2 MLX and its private runtime from Settings. Setup needs an internet connection; dictation runs offline.".into(),
        })
    }
}

fn directory_bytes(root: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| {
            // Do not follow Python's symlinks or count the interpreter twice.
            let Ok(kind) = entry.file_type() else {
                return 0;
            };
            if kind.is_dir() {
                directory_bytes(&entry.path())
            } else if kind.is_file() {
                entry.metadata().map(|m| m.len()).unwrap_or(0)
            } else {
                0
            }
        })
        .sum()
}

impl EngineTranscriber for MlxProvider {
    fn engine(&self) -> TranscriptionEngine {
        TranscriptionEngine::Phonon
    }
    fn metadata(&self) -> EngineMetadata {
        EngineMetadata {
            engine: self.engine(), model_id: "FermionResearch/Phonon-2",
            capability: "Phonon-2 running on this Mac's GPU through Apple's MLX. Runs offline on Apple silicon with macOS 14 or later.",
            revision: "FermionResearch/Phonon-2@ca1bef26bcd8ef4a7e16d0636d8a77bb25e298ee; fermion-research 0.2.7 / MLX 0.31.1",
            approximate_bytes: Some(1_200_000_000), source_url: Some(SOURCE),
            license: "CC BY 4.0", license_url: "https://creativecommons.org/licenses/by/4.0/",
            attribution: Some("Speech recognition by Phonon-2 (Fermion Research), derived from NVIDIA Parakeet TDT 0.6B v3 (© NVIDIA Corporation); both used under CC BY 4.0."),
            modifications: Some("Fermion's five-value Phonon-2 weights and MLX runtime. Slugtale installs the official model unchanged, with a private Python runtime. Inference uses Fermion's tdt16,dense16 speed settings and stays on this Mac. Runtime code is Apache 2.0; dependency notices are installed with each package."),
            system_managed: false, supported_platforms: "Apple silicon macOS 14 or later (MLX); other platforms use ONNX",
        }
    }
    fn availability(&self) -> EngineAvailability {
        lock(&self.availability).clone()
    }
    fn warm_up(&self) -> Result<(), AsrError> {
        self.with_worker(|_| Ok(()))
    }
    fn transcribe(&self, audio: &CapturedAudio) -> Result<EngineTranscription, AsrError> {
        if audio.sample_rate_hz != 16_000
            || audio.samples.is_empty()
            || audio.samples.len() > MAX_SAMPLES
            || audio.samples.iter().any(|x| !x.is_finite())
        {
            return Err(AsrError::UnsupportedAudio(
                "Phonon-2 needs finite 16 kHz mono samples, from one sample to 30 minutes.".into(),
            ));
        }
        let start = Instant::now();
        let transcription = self.with_worker(|worker| worker.transcribe(&audio.samples))?;
        Ok(EngineTranscription::plain(
            self.engine(),
            transcription,
            start.elapsed(),
        ))
    }
}

/// Phonon-2 MLX is the one engine whose assets Slugtale installs from Settings:
/// a private Python runtime plus the weights, downloaded and set up on demand.
/// Installing and removing is the engine's own job, so it lives here and not on
/// the transcriber above.
impl EngineAssetLifecycle for MlxProvider {
    fn assets(&self) -> EngineAssets {
        EngineAssets {
            installed_bytes: Some(directory_bytes(&self.root)),
            present: Some(probe(&self.root).is_available()),
        }
    }
    fn can_install_assets(&self) -> bool {
        true
    }
    fn install_assets(
        &self,
        on_progress: &mut dyn FnMut(DownloadProgress),
    ) -> Result<AssetInstall, String> {
        self.install_with(&HttpModelDownloader, on_progress)
    }
    fn remove_assets(&self) -> Result<(), String> {
        let _gate = lock(&self.install_gate);
        self.begin_change();
        let result = if self.root.exists() {
            std::fs::remove_dir_all(&self.root)
        } else {
            Ok(())
        };
        self.finish_change();
        result.map_err(|_| "Could not remove Phonon-2 MLX. Close Slugtale and try again.".into())
    }
}

/// Owns a process group, including pip if setup times out. Always reap it.
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        // try_wait may already have reaped an exited child. Never signal a
        // PID after that: the OS can reuse it for an unrelated process.
        if matches!(self.0.try_wait(), Ok(None)) {
            unsafe {
                kill(-(self.0.id() as i32), 9);
            }
            let _ = self.0.wait();
        }
    }
}

/// The one Python process that an install step runs. Spawn, reap, and close
/// share a lock, so a PID is never signalled after it is reaped, and no step
/// starts once shutdown has closed this.
#[derive(Default)]
struct Stop {
    state: Mutex<StopState>,
}

#[derive(Default)]
struct StopState {
    closed: bool,
    child: Option<Process>,
}

impl Stop {
    fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    fn spawn(&self, command: &mut Command) -> Result<(), String> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(SHUTTING_DOWN.into());
        }
        if state.child.is_some() {
            return Err("Could not start Phonon-2 setup.".into());
        }
        let child = command
            .spawn()
            .map_err(|_| "Could not start Phonon-2 setup.".to_string())?;
        state.child = Some(Process(child));
        Ok(())
    }

    /// Reaps the step once it has exited, so its PID is never signalled later.
    fn poll(&self) -> Result<Option<ExitStatus>, String> {
        let mut state = lock(&self.state);
        let Some(child) = state.child.as_mut() else {
            return Err(SHUTTING_DOWN.into());
        };
        match child.0.try_wait() {
            Ok(Some(status)) => {
                state.child.take();
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(_) => Err("Could not check Phonon-2 setup.".into()),
        }
    }

    fn finish(&self) {
        lock(&self.state).child.take();
    }

    /// Refuses further steps and kills and reaps the running one before it
    /// returns. Never waits for the installer thread or its locks.
    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.child.take();
    }
}

fn offline_command(root: &Path) -> Command {
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.args(["-p", SANDBOX])
        .arg(root.join("python/bin/python3"))
        .args(["-I", "-u", "-c", WORKER])
        .arg(root);
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("TMPDIR", std::env::temp_dir())
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1")
        .env("FERMION_DEVICE", "mlx")
        .env("FERMION_P2_FAST", "tdt16,dense16")
        .stderr(Stdio::null())
        .process_group(0);
    cmd
}

struct Worker {
    process: Process,
    channel: BufReader<UnixStream>,
}
impl Worker {
    fn start(root: &Path) -> Result<Self, AsrError> {
        Self::spawn(offline_command(root))
    }
    fn spawn(mut command: Command) -> Result<Self, AsrError> {
        let (channel, input, output) = socket_pair()?;
        let process = command
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .spawn()
            .map_err(|_| runtime_error())?;
        let mut worker = Self {
            process: Process(process),
            channel,
        };
        await_ready(&mut worker.channel)?;
        Ok(worker)
    }
    fn response<T: serde::de::DeserializeOwned>(
        &mut self,
        deadline: Instant,
    ) -> Result<T, AsrError> {
        read_reply(&mut self.channel, deadline)
    }
    fn transcribe(&mut self, samples: &[f32]) -> Result<FinalTranscription, AsrError> {
        if self
            .process
            .0
            .try_wait()
            .map_err(|_| runtime_error())?
            .is_some()
        {
            return Err(runtime_error());
        }
        let deadline = Instant::now() + START_TIMEOUT;
        let channel = self.channel.get_mut();
        write_until(channel, &(samples.len() as u32).to_le_bytes(), deadline)?;
        // Bounded scratch space, including long clips; no audio file on disk.
        let mut bytes = Vec::with_capacity(64 * 1024);
        for chunk in samples.chunks(16 * 1024) {
            bytes.clear();
            for sample in chunk {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            write_until(channel, &bytes, deadline)?;
        }
        self.response(deadline)
    }
}

type Channel = BufReader<UnixStream>;

/// Returns the parent end of a worker socket, and the child's stdin and stdout.
fn socket_pair() -> Result<(Channel, OwnedFd, OwnedFd), AsrError> {
    let (parent, child) = UnixStream::pair().map_err(|_| runtime_error())?;
    parent
        .set_read_timeout(Some(START_TIMEOUT))
        .map_err(|_| runtime_error())?;
    parent
        .set_write_timeout(Some(START_TIMEOUT))
        .map_err(|_| runtime_error())?;
    let input: OwnedFd = child.into();
    let output = input.try_clone().map_err(|_| runtime_error())?;
    Ok((BufReader::new(parent), input, output))
}

fn await_ready(channel: &mut Channel) -> Result<(), AsrError> {
    let ready: serde_json::Value = read_reply(channel, Instant::now() + START_TIMEOUT)?;
    if ready.get("ready") == Some(&serde_json::Value::Bool(true)) {
        Ok(())
    } else {
        Err(runtime_error())
    }
}

fn read_reply<T: serde::de::DeserializeOwned>(
    channel: &mut Channel,
    deadline: Instant,
) -> Result<T, AsrError> {
    let mut bytes = Vec::new();
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(runtime_error)?;
        channel
            .get_ref()
            .set_read_timeout(Some(remaining))
            .map_err(|_| runtime_error())?;
        let part = channel.fill_buf().map_err(|_| runtime_error())?;
        if part.is_empty() {
            return Err(runtime_error());
        }
        let newline = part.iter().position(|byte| *byte == b'\n');
        let count = newline.map_or(part.len(), |index| index + 1);
        if bytes.len() + count > MAX_RESPONSE as usize {
            return Err(runtime_error());
        }
        bytes.extend_from_slice(&part[..count]);
        channel.consume(count);
        if newline.is_some() {
            return serde_json::from_slice(&bytes).map_err(|_| runtime_error());
        }
    }
}

/// Starts the installed runtime once. Its process stays in `stop` during the
/// handshake, so shutdown can stop it, and it is reaped before this returns.
fn check_runtime(root: &Path, stop: &Stop) -> Result<(), AsrError> {
    let (mut channel, input, output) = socket_pair()?;
    let mut command = offline_command(root);
    command.stdin(Stdio::from(input)).stdout(Stdio::from(output));
    let spawned = stop.spawn(&mut command).map_err(|_| runtime_error());
    // Drop the child's socket ends here, so a dead child gives EOF at once.
    drop(command);
    let ready = spawned.and_then(|()| await_ready(&mut channel));
    stop.finish();
    ready
}

fn write_until(
    channel: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), AsrError> {
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(runtime_error)?;
        channel
            .set_write_timeout(Some(remaining))
            .map_err(|_| runtime_error())?;
        let count = channel.write(bytes).map_err(|_| runtime_error())?;
        if count == 0 {
            return Err(runtime_error());
        }
        bytes = &bytes[count..];
    }
    Ok(())
}

struct Artifact {
    name: &'static str,
    bytes: u64,
    sha: &'static str,
}
const MODEL_FILES: &[Artifact] = &[
    Artifact {
        name: "phonon-2.bps.tar.zst",
        bytes: 163_515_201,
        sha: "98125795b6dda72f5c6eee9ba33d19815df65dcb18b50a357bf9f73c9935309e",
    },
    Artifact {
        name: "config.json",
        bytes: 759,
        sha: "422379e411f14dde97174554deead106ec1e4816e14dd78a1bf99517c6dcc663",
    },
    Artifact {
        name: "packed_manifest.json",
        bytes: 699,
        sha: "690c6bc43bcbcae8df61cff0b2cace7299d023718b6f2aa9d67157407eadaa07",
    },
    Artifact {
        name: "NOTICE",
        bytes: 2741,
        sha: "00624a5043e7ce74029317b024132ca5286116d6fbf3191d226374bc8273789f",
    },
    Artifact {
        name: "LICENSE-CODE-Apache-2.0.txt",
        bytes: 11358,
        sha: "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
    },
    Artifact {
        name: "LICENSE-WEIGHTS-CC-BY-4.0.txt",
        bytes: 18657,
        sha: "9ba9550ad48438d0836ddab3da480b3b69ffa0aac7b7878b5a0039e7ab429411",
    },
];
const PYTHON: Artifact = Artifact {
    name: "python.tar.gz",
    bytes: 25_160_994,
    sha: "f1ee170bd7bb45bea526c4f9489b41f9c5d978c4cd7a08fd11809de56c39b736",
};

fn verify(path: &Path, artifact: &Artifact) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    if !file.metadata().is_ok_and(|m| m.len() == artifact.bytes) {
        return false;
    }
    let mut digest = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        match file.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => digest.update(&bytes[..n]),
            Err(_) => return false,
        }
    }
    format!("{:x}", digest.finalize()) == artifact.sha
}

fn download(
    root: &Path,
    artifact: &Artifact,
    url: &str,
    downloader: &dyn ModelDownloader,
    progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), String> {
    let path = root.join(artifact.name);
    if verify(&path, artifact) {
        return Ok(());
    }
    let staging = path.with_extension("download");
    let result = (|| {
        downloader.download(url, &staging, progress).map_err(|_| {
            "Phonon-2 setup download failed. Check your connection and try Install again."
                .to_string()
        })?;
        if !verify(&staging, artifact) {
            return Err("Phonon-2 setup checksum failed. Try Install again.".into());
        }
        std::fs::rename(&staging, &path).map_err(|_| "Could not save Phonon-2 setup files.".into())
    })();
    let _ = std::fs::remove_file(staging);
    result
}

const SHUTTING_DOWN: &str = "Phonon-2 is shutting down.";

fn cancelled(stop: &Stop) -> Result<(), String> {
    if stop.is_closed() {
        Err(SHUTTING_DOWN.into())
    } else {
        Ok(())
    }
}

fn run_setup(cmd: &mut Command, timeout: Duration, stop: &Stop) -> Result<(), String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    stop.spawn(cmd)?;
    let start = Instant::now();
    loop {
        if let Some(status) = stop.poll()? {
            return if status.success() {
                Ok(())
            } else {
                Err("Phonon-2 setup failed. Check free disk space and your connection, then try Install again.".into())
            };
        }
        if start.elapsed() > timeout {
            stop.finish();
            return Err("Phonon-2 setup took too long. Try Install again.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn install(
    root: &Path,
    downloader: &dyn ModelDownloader,
    stop: &Stop,
    progress: &mut dyn FnMut(DownloadProgress),
) -> Result<(), String> {
    if probe(root).is_available() {
        return Ok(());
    }
    let io_error = |_| "Could not save Phonon-2 setup files. Check free disk space.".to_string();
    std::fs::create_dir_all(root).map_err(io_error)?;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    let _ = std::fs::remove_file(root.join("installed"));
    cancelled(stop)?;
    download(root, &PYTHON, PYTHON_URL, downloader, progress)?;
    cancelled(stop)?;
    run_setup(
        Command::new("/usr/bin/tar")
            .arg("-xzf")
            .arg(root.join(PYTHON.name))
            .arg("-C")
            .arg(root),
        Duration::from_secs(60),
        stop,
    )?;
    for file in MODEL_FILES {
        let url = format!(
            "https://huggingface.co/FermionResearch/Phonon-2/resolve/{REVISION}/{}",
            file.name
        );
        cancelled(stop)?;
        download(root, file, &url, downloader, progress)?;
    }
    progress(DownloadProgress {
        downloaded: 0,
        total: None,
    });
    std::fs::write(root.join("requirements.lock"), REQUIREMENTS).map_err(io_error)?;
    // Start fresh after any interrupted pip install; no half-ready packages.
    if root.join("site").exists() {
        std::fs::remove_dir_all(root.join("site")).map_err(io_error)?;
    }
    let mut cmd = Command::new(root.join("python/bin/python3"));
    cmd.args(["-I", "-c", SETUP])
        .arg(root)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("TMPDIR", std::env::temp_dir())
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .env("DO_NOT_TRACK", "1");
    cancelled(stop)?;
    run_setup(&mut cmd, Duration::from_secs(960), stop)?;
    // Test the actual sandboxed runtime before declaring the install ready.
    cancelled(stop)?;
    check_runtime(root, stop)
        .map_err(|_| "Phonon-2 MLX could not load. Try Install again.".to_string())?;
    cancelled(stop)?;
    std::fs::write(root.join("installed"), install_stamp()).map_err(io_error)?;
    for name in [PYTHON.name, "phonon-2.bps.tar.zst", "requirements.lock"] {
        let _ = std::fs::remove_file(root.join(name));
    }
    progress(DownloadProgress {
        downloaded: 1,
        total: Some(1),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("slugtale-mlx-{name}-{}", std::process::id()))
    }

    fn fake_worker() -> (Worker, UnixStream) {
        let (parent, peer) = UnixStream::pair().unwrap();
        parent
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let process = Command::new("/bin/sleep")
            .arg("10")
            .process_group(0)
            .spawn()
            .unwrap();
        (
            Worker {
                process: Process(process),
                channel: BufReader::new(parent),
            },
            peer,
        )
    }

    #[test]
    fn missing_or_partial_setup_never_claims_to_be_ready() {
        let root = root("partial");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("installed"), install_stamp()).unwrap();
        let provider = MlxProvider::new(root.clone());
        assert!(!provider.availability().is_available());
        assert!(crate::EngineView::of(&provider, false).installable);
        assert!(provider.warm_up().is_err());
        assert!(lock(&provider.worker).is_none());
        provider.remove_assets().unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn shutdown_rejects_late_warm_up() {
        let provider = MlxProvider::new(root("shutdown"));
        provider.shutdown();
        assert!(matches!(provider.warm_up(), Err(AsrError::Runtime(_))));
        assert!(lock(&provider.worker).is_none());
    }

    #[test]
    fn invalid_audio_fails_before_loading_any_model() {
        let provider = MlxProvider::new(root("invalid"));
        for audio in [
            CapturedAudio {
                sample_rate_hz: 8000,
                samples: vec![0.0],
            },
            CapturedAudio {
                sample_rate_hz: 16000,
                samples: vec![],
            },
            CapturedAudio {
                sample_rate_hz: 16000,
                samples: vec![f32::NAN],
            },
        ] {
            assert!(matches!(
                provider.transcribe(&audio),
                Err(AsrError::UnsupportedAudio(_))
            ));
        }
        assert!(lock(&provider.worker).is_none());
    }

    #[test]
    fn stalled_or_crashed_worker_returns_a_fixed_content_free_error() {
        let (mut worker, mut peer) = fake_worker();
        // A live process which never replies has a bounded wait.
        assert!(worker
            .response::<FinalTranscription>(Instant::now() + Duration::from_millis(20))
            .is_err());
        peer.write_all(b"{\"error\":\"private spoken words\"}\n")
            .unwrap();
        let error = worker
            .response::<FinalTranscription>(Instant::now() + Duration::from_millis(20))
            .unwrap_err()
            .to_string();
        assert!(!error.contains("private spoken words"));
        drop(peer);
        assert!(worker
            .response::<FinalTranscription>(Instant::now() + Duration::from_millis(20))
            .is_err());
    }

    #[test]
    fn a_worker_that_does_not_read_has_a_bounded_send() {
        let (mut channel, _peer) = UnixStream::pair().unwrap();
        assert!(write_until(
            &mut channel,
            &vec![0; 2 * 1024 * 1024],
            Instant::now() + Duration::from_millis(20)
        )
        .is_err());
    }

    #[test]
    fn response_size_is_bounded() {
        let (mut worker, mut peer) = fake_worker();
        let writer = std::thread::spawn(move || {
            let _ = peer.write_all(&vec![b'x'; MAX_RESPONSE as usize + 2]);
        });
        assert!(worker
            .response::<FinalTranscription>(Instant::now() + Duration::from_millis(20))
            .is_err());
        drop(worker);
        writer.join().unwrap();
    }

    #[test]
    fn a_failed_decode_releases_the_process_for_a_clean_retry() {
        let provider = MlxProvider::new(root("failure"));
        let (worker, _peer) = fake_worker();
        *lock(&provider.worker) = Some(worker);
        let answer: Result<(), _> = provider.with_worker(|_| Err(runtime_error()));
        assert!(answer.is_err());
        assert!(lock(&provider.worker).is_none());
    }

    #[test]
    fn removal_waits_for_an_in_flight_operation_and_stops_the_worker() {
        let root = root("removal");
        std::fs::create_dir_all(&root).unwrap();
        let provider = Arc::new(MlxProvider::new(root.clone()));
        let (worker, _peer) = fake_worker();
        *lock(&provider.worker) = Some(worker);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let decoding = provider.clone();
        let decode = std::thread::spawn(move || {
            decoding.with_worker(|_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        entered_rx.recv().unwrap();
        let removing = provider.clone();
        let (removed_tx, removed_rx) = std::sync::mpsc::channel();
        let remove = std::thread::spawn(move || {
            removing.remove_assets().unwrap();
            removed_tx.send(()).unwrap();
        });
        assert!(removed_rx.recv_timeout(Duration::from_millis(20)).is_err());
        assert!(root.exists());
        release_tx.send(()).unwrap();
        decode.join().unwrap().unwrap();
        remove.join().unwrap();
        assert!(lock(&provider.worker).is_none());
        assert!(!root.exists());
        assert!(!provider.availability().is_available());
    }

    #[test]
    fn a_complete_runtime_is_available_only_with_its_stamp_and_full_container() {
        let root = root("complete");
        for dir in ["python/bin", "site/fermion/_speech", "model"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("python/bin/python3"), b"").unwrap();
        std::fs::write(root.join("site/fermion/_speech/engine_phonon2.py"), b"").unwrap();
        std::fs::write(root.join("model/config.json"), b"{}").unwrap();
        let container = root.join("model/model.fermion");
        std::fs::File::create(&container)
            .unwrap()
            .set_len(177_438_361)
            .unwrap();
        std::fs::write(root.join("installed"), install_stamp()).unwrap();
        assert!(probe(&root).is_available());

        std::fs::write(root.join("installed"), "an older install").unwrap();
        assert!(!probe(&root).is_available());
        std::fs::write(root.join("installed"), install_stamp()).unwrap();
        std::fs::File::create(&container)
            .unwrap()
            .set_len(177_438_360)
            .unwrap();
        assert!(!probe(&root).is_available());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transcribe_sends_little_endian_samples_and_reads_the_text_reply() {
        let (mut worker, mut peer) = fake_worker();
        let fake = std::thread::spawn(move || {
            let mut count = [0; 4];
            peer.read_exact(&mut count).unwrap();
            assert_eq!(u32::from_le_bytes(count), 3);
            let mut bytes = [0; 12];
            peer.read_exact(&mut bytes).unwrap();
            let samples: Vec<f32> = bytes
                .chunks(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().unwrap()))
                .collect();
            assert_eq!(samples, vec![0.25, -0.5, 1.0]);
            peer.write_all(b"{\"text\":\"hello there\",\"segments\":[]}\n")
                .unwrap();
        });
        let transcription = worker.transcribe(&[0.25, -0.5, 1.0]).unwrap();
        fake.join().unwrap();
        assert_eq!(transcription.text, "hello there");
    }

    /// A stand-in runtime: real worker.py, with stub numpy and fermion packages
    /// that answer from the samples alone. No model, network, or sandbox.
    fn stub_runtime(name: &str) -> PathBuf {
        let root = root(name);
        let site = root.join("site");
        let write = |path: &str, text: &str| {
            let file = site.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write(
            "fermion_research-0.2.7.dist-info/METADATA",
            "Metadata-Version: 2.1\nName: fermion-research\nVersion: 0.2.7\n",
        );
        write(
            "numpy/__init__.py",
            r#"import math, struct
float32 = "float32"
class Array(list):
    def all(self):
        return all(self)
def zeros(count, dtype=None):
    return Array([0.0] * count)
def frombuffer(data, dtype=None):
    return Array(struct.unpack("<%df" % (len(data) // 4), bytes(data)))
def isfinite(values):
    return Array(math.isfinite(value) for value in values)
"#,
        );
        write("fermion/__init__.py", "");
        write("fermion/_speech/__init__.py", "");
        write(
            "fermion/_speech/engine_phonon2.py",
            r#"class Result:
    def __init__(self, text):
        self.text = text
class Model:
    decode = {"levers_error": None}
    def transcribe_array(self, audio):
        return None
    def transcribe_array_detailed(self, audio):
        if len(audio) and audio[0] == 99.0:
            raise RuntimeError("private spoken words")
        return Result("heard %d samples" % len(audio))
def load(path, *, profile, backend, quiet):
    return Model()
"#,
        );
        root
    }

    fn stub_worker(root: &Path) -> Worker {
        let mut command = Command::new("/usr/bin/python3");
        command
            .args(["-I", "-u", "-c", WORKER])
            .arg(root)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stderr(Stdio::null())
            .process_group(0);
        Worker::spawn(command).unwrap()
    }

    #[test]
    fn worker_frames_audio_and_returns_only_the_text_reply() {
        let root = stub_runtime("worker-frames");
        let mut worker = stub_worker(&root);
        let transcription = worker.transcribe(&[0.0, 0.5, 1.0]).unwrap();
        assert_eq!(transcription.text, "heard 3 samples");
        drop(worker);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_failure_is_redacted_and_the_process_exits() {
        let root = stub_runtime("worker-redacted");
        let mut worker = stub_worker(&root);
        let error = worker.transcribe(&[99.0]).unwrap_err().to_string();
        assert!(!error.contains("private spoken words"));
        assert!(!worker.process.0.wait().unwrap().success());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_exits_cleanly_when_its_input_closes() {
        let root = stub_runtime("worker-eof");
        let mut worker = stub_worker(&root);
        worker
            .channel
            .get_ref()
            .shutdown(std::net::Shutdown::Both)
            .unwrap();
        assert!(worker.process.0.wait().unwrap().success());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct BlockingDownload {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl ModelDownloader for BlockingDownload {
        fn download(
            &self,
            _: &str,
            _: &Path,
            _: &mut dyn FnMut(DownloadProgress),
        ) -> Result<(), crate::ModelError> {
            self.entered.send(()).unwrap();
            self.release.recv().unwrap();
            Err(std::io::Error::other("released without a file").into())
        }
    }

    /// Starts an install that is parked inside its first download, with no
    /// network involved. Send on the returned channel to let the download fail.
    fn blocked_install(
        provider: &Arc<MlxProvider>,
    ) -> (
        std::thread::JoinHandle<Result<AssetInstall, String>>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let installing = provider.clone();
        let install = std::thread::spawn(move || {
            installing.install_with(
                &BlockingDownload {
                    entered: entered_tx,
                    release: release_rx,
                },
                &mut |_| {},
            )
        });
        entered_rx.recv().unwrap();
        (install, release_tx)
    }

    #[test]
    fn shutdown_returns_promptly_during_an_install() {
        let root = root("shutdown-install");
        let provider = Arc::new(MlxProvider::new(root.clone()));
        let (install, release) = blocked_install(&provider);
        assert!(matches!(
            provider.warm_up(),
            Err(AsrError::EngineUnavailable { .. })
        ));
        let started = Instant::now();
        provider.shutdown();
        assert!(started.elapsed() < Duration::from_millis(100));
        release.send(()).unwrap();
        assert!(install.join().unwrap().is_err());
        assert!(lock(&provider.worker).is_none());
        assert!(matches!(provider.warm_up(), Err(AsrError::Runtime(_))));
        let _ = std::fs::remove_dir_all(root);
    }

    fn process_running(pid: u32) -> bool {
        let out = Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let state = String::from_utf8_lossy(&out.stdout);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }

    #[test]
    fn shutdown_kills_a_running_setup_step_and_its_descendants_before_returning() {
        let root = root("shutdown-setup");
        std::fs::create_dir_all(&root).unwrap();
        let pid_file = root.join("descendant.pid");
        let script = format!(
            "sleep 30 & echo $! > '{0}.tmp' && mv '{0}.tmp' '{0}'; wait",
            pid_file.display()
        );
        let provider = Arc::new(MlxProvider::new(root.clone()));
        let setup = provider.clone();
        let running = std::thread::spawn(move || {
            run_setup(
                Command::new("/bin/sh").args(["-c", &script]),
                Duration::from_secs(60),
                &setup.stop,
            )
        });
        let descendant = (0..400)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(10));
                std::fs::read_to_string(&pid_file)
                    .ok()?
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .expect("setup started a descendant");
        let started = Instant::now();
        provider.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!process_running(descendant));
        assert!(lock(&provider.stop.state).child.is_none());
        assert_eq!(running.join().unwrap(), Err(SHUTTING_DOWN.to_string()));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_setup_step_started_after_shutdown_is_refused() {
        let root = root("late-setup");
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("started");
        let provider = MlxProvider::new(root.clone());
        provider.shutdown();
        let result = run_setup(
            Command::new("/usr/bin/touch").arg(&marker),
            Duration::from_secs(5),
            &provider.stop,
        );
        assert_eq!(result, Err(SHUTTING_DOWN.to_string()));
        assert!(!marker.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn shutdown_stops_the_runtime_check_during_its_handshake() {
        let root = root("shutdown-check");
        let bin = root.join("python/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let python = bin.join("python3");
        std::fs::write(&python, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = Arc::new(MlxProvider::new(root.clone()));
        let checking = provider.clone();
        let check = std::thread::spawn(move || check_runtime(&checking.root, &checking.stop));
        for _ in 0..400 {
            if lock(&provider.stop.state).child.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let started = Instant::now();
        provider.shutdown();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(check.join().unwrap().is_err());
        assert!(lock(&provider.stop.state).child.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    struct BrokenDownload;
    impl ModelDownloader for BrokenDownload {
        fn download(
            &self,
            _: &str,
            destination: &Path,
            _: &mut dyn FnMut(DownloadProgress),
        ) -> Result<(), crate::ModelError> {
            std::fs::write(destination, b"wrong")?;
            Ok(())
        }
    }

    #[test]
    fn corrupt_download_is_discarded_and_is_never_installed() {
        let root = root("download");
        std::fs::create_dir_all(&root).unwrap();
        let artifact = Artifact {
            name: "fixture",
            bytes: 5,
            sha: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        };
        assert!(download(
            &root,
            &artifact,
            "https://example.invalid",
            &BrokenDownload,
            &mut |_| {}
        )
        .is_err());
        assert!(!root.join("fixture").exists());
        assert!(!root.join("fixture.download").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
