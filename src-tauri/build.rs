use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    tauri_build::build();
    build_apple_speech_bridge();
}

/// The lowest macOS this bridge's object code claims to support.
///
/// It is not 26 even though `SpeechTranscriber` is: every entry point in the
/// Swift file checks `#available(macOS 26.0, *)` first, so one build runs on
/// older Macs and reports `UnsupportedOsVersion` instead of refusing to launch.
/// It is not lower than 13 either — below macOS 12 a Swift binary that uses
/// structured concurrency needs the `swiftCompatibilityConcurrency` back-deploy
/// archives, which live inside the toolchain rather than on the user's machine
/// and which the Rust link line has no business hunting for. 13 is the first
/// version comfortably clear of that cliff.
const APPLE_SPEECH_DEPLOYMENT_TARGET: &str = "13.0";

/// Where macOS keeps the Swift runtime it ships with the OS. The static archive
/// swiftc produces carries autolink directives for `swiftCore` and friends; this
/// is the search path that lets the linker resolve them without bundling a
/// second copy of the Swift standard library into Slugtale.
const MACOS_SWIFT_RUNTIME_DIR: &str = "/usr/lib/swift";

/// Compile the Swift half of the Apple SpeechTranscriber engine
/// (`swift/SlugtaleAppleSpeech.swift`) into a static archive and link it in.
///
/// This exists because macOS 26's `SpeechAnalyzer` / `SpeechTranscriber` API is
/// Swift-only — it is not exported to Objective-C, so no `objc2-*` crate can
/// reach it. A small Swift static library with a C ABI is the only supported
/// route from Rust, and that means the Swift compiler becomes a build-time
/// dependency. Which is exactly why the whole step is behind an opt-in Cargo
/// feature: a plain `cargo test`, and every Linux and Windows build, must not
/// need a Swift toolchain to exist.
fn build_apple_speech_bridge() {
    // Cargo's default "rerun when anything in the package changes" is switched
    // off the moment any `rerun-if-changed` is emitted, and `tauri_build::build`
    // emits several. Declare the Swift source unconditionally so a build that
    // later turns the feature on still sees edits made while it was off.
    println!("cargo:rerun-if-changed=swift/SlugtaleAppleSpeech.swift");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");

    // Both gates, in the same order as `src/apple_speech.rs`: the feature can be
    // enabled on Linux or Windows without breaking the build, it simply does
    // nothing there and the provider reports an unsupported platform.
    if std::env::var_os("CARGO_FEATURE_APPLE_SPEECH_RUNTIME").is_none() {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let archive = out_dir.join("libslugtale_apple_speech.a");
    let source = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets this"))
        .join("swift")
        .join("SlugtaleAppleSpeech.swift");

    compile_swift_archive(&source, &archive);

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    println!("cargo:rustc-link-lib=static=slugtale_apple_speech");
    println!("cargo:rustc-link-search=native={MACOS_SWIFT_RUNTIME_DIR}");
    // Swift's concurrency runtime is the one library the toolchain records as
    // `@rpath/libswift_Concurrency.dylib` rather than by absolute path, because
    // it is also shipped as a back-deployment copy inside apps that target macOS
    // 11. Slugtale targets 13 and wants the operating system's copy, so it needs
    // a run path to find it — without this the binary links cleanly and then
    // dies at launch with "Library not loaded".
    println!("cargo:rustc-link-arg=-Wl,-rpath,{MACOS_SWIFT_RUNTIME_DIR}");
    // Speech, AVFoundation, Foundation, and CoreMedia are not named here on
    // purpose: swiftc records them as `LC_LINKER_OPTION` autolink directives
    // inside the archive's objects, so the linker picks them up itself. Listing
    // them again would be a second, silently drifting copy of the truth.
}

fn compile_swift_archive(source: &Path, archive: &Path) {
    let deployment_target = std::env::var("MACOSX_DEPLOYMENT_TARGET")
        .unwrap_or_else(|_| APPLE_SPEECH_DEPLOYMENT_TARGET.to_string());
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(other) => panic!(
            "the apple-speech-runtime feature has no Swift target for the {other} architecture"
        ),
        Err(_) => panic!("cargo always sets CARGO_CFG_TARGET_ARCH"),
    };

    // `xcrun` picks the active toolchain and SDK the same way Xcode would, so a
    // machine with several Xcodes installed compiles the bridge against the one
    // the developer actually selected.
    let mut swiftc = Command::new("xcrun");
    swiftc
        .arg("swiftc")
        .arg("-emit-library")
        // A static archive rather than a dylib: one binary to run, nothing extra
        // to find at load time, and no install-name to fix up in the bundle.
        .arg("-static")
        .arg("-module-name")
        .arg("SlugtaleAppleSpeech")
        .arg("-target")
        .arg(format!("{arch}-apple-macosx{deployment_target}"))
        // Optimised in every profile. The bridge is a few hundred lines whose
        // cost is dominated by the Speech framework behind it, and building it
        // identically in debug and release removes a class of "only reproduces
        // in release" report from a component that is already hard to observe.
        .arg("-O")
        .arg("-wmo")
        .arg("-o")
        .arg(archive)
        .arg(source);

    let output = swiftc.output().unwrap_or_else(|error| {
        panic!(
            "could not run `xcrun swiftc` to build the Apple SpeechTranscriber bridge: {error}.\n\
             The apple-speech-runtime feature needs the Xcode command line tools \
             (`xcode-select --install`)."
        )
    });

    if !output.status.success() {
        panic!(
            "compiling the Apple SpeechTranscriber bridge failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}
