// The Swift half of the Apple SpeechTranscriber Transcription Engine
// (slugtale-vjs.2).
//
// Why this file exists at all: macOS 26's `SpeechAnalyzer` / `SpeechTranscriber`
// API is Swift-only. It is not exported to Objective-C, so no `objc2-*` crate
// can reach it — `objc2-speech` only binds the legacy `SFSpeechRecognizer`,
// which is a different, older engine with a server fallback we explicitly do
// not want. The only supported route from Rust is a small Swift library with a
// C ABI, which is what this is. `build.rs` compiles it into a static archive
// and links it into the Rust binary; `src/apple_speech.rs` declares the same
// five functions and owns every policy decision above them.
//
// Design rules this file follows, in order of importance:
//
//  1. **Nothing leaves the device.** `SpeechAnalyzer` has no server mode at all
//     — unlike `SFSpeechRecognizer` there is no `requiresOnDeviceRecognition`
//     switch to get wrong, because there is no remote path to switch away from.
//     On top of that, `transcribe` refuses to run unless
//     `AssetInventory.status(forModules:)` reports `.installed`, so a machine
//     that is still fetching assets is reported unavailable rather than being
//     allowed to reach for anything else.
//  2. **Nothing is logged.** No `print`, no `os_log`, no `NSLog` anywhere in
//     this file. Transcripts, alternatives, confidence, and the audio itself
//     exist only in the buffers handed straight back to Rust. Detail strings
//     returned alongside a status describe the machine, never the speech.
//  3. **Apple's assets stay Apple's.** Nothing here reads, copies, or exports
//     model files; installation goes through `AssetInventory`'s own request
//     object, and only when Rust calls `slugtale_apple_speech_install_assets`
//     from an explicit user action.
//  4. **The ABI is boring on purpose.** Only `Int32`, `Double`, and
//     `malloc`-owned C strings cross the boundary. Swift struct layout is not
//     guaranteed to match a Rust `#[repr(C)]` struct, so this file passes no
//     structs — every value comes back through its own out-parameter, and every
//     string is freed by `slugtale_apple_speech_free`.
//
// The whole Speech API used here is annotated `@available(macOS 26.0, *)`, so
// the library is compiled against an older deployment target and every entry
// point checks `#available` first. That is what lets one build of Slugtale run
// on macOS 13 and report `UNSUPPORTED_OS` instead of failing to launch.

import AVFoundation
import Foundation
import Speech

// MARK: - Status codes

// These are the contract with `apple_speech.rs`; the Rust side maps each one to
// an `EngineUnavailable` variant. Add cases at the end, never renumber.

/// The call succeeded.
private let STATUS_OK: Int32 = 0
/// This machine runs a macOS older than 26, where the API does not exist.
/// Detail: the detected OS version, e.g. `macOS 15.3.1`.
private let STATUS_UNSUPPORTED_OS: Int32 = 1
/// `SpeechTranscriber` cannot transcribe the requested Dictation Language.
/// Detail: the locale identifier that was asked for.
private let STATUS_UNSUPPORTED_LOCALE: Int32 = 2
/// The locale is supported but its system assets are not installed yet. This is
/// the one recoverable status; Rust turns it into an install action.
private let STATUS_ASSETS_MISSING: Int32 = 3
/// Probing or transcription failed for a reason none of the others cover.
private let STATUS_FAILED: Int32 = 4
/// macOS is new enough but this hardware cannot run the on-device transcriber.
private let STATUS_UNSUPPORTED_HARDWARE: Int32 = 5

/// Written to the confidence out-parameters when `SpeechTranscriber` reported no
/// confidence at all. A silent engine is not a low-confidence engine, and the
/// Second Opinion router has to be able to tell the two apart, so the sentinel
/// sits outside the valid 0.0...1.0 range rather than at its floor.
private let CONFIDENCE_UNREPORTED: Double = -1.0

/// Separates whole-transcript alternatives in the single string handed back to
/// Rust. ASCII RS (0x1E) is a control character no transcriber emits, which
/// keeps the ABI to one owned pointer instead of an array of arrays.
private let ALTERNATIVE_SEPARATOR = "\u{1E}"

// MARK: - C ABI

/// Whether Apple SpeechTranscriber could ever transcribe on this machine, as a
/// cheap yes/no for callers that do not need a reason. Returns 1 when the OS is
/// new enough and the hardware supports the on-device transcriber, 0 otherwise.
/// It says nothing about locales or installed assets — use
/// `slugtale_apple_speech_probe` for that.
@_cdecl("slugtale_apple_speech_available")
public func slugtale_apple_speech_available() -> Int32 {
    if #available(macOS 26.0, *) {
        return SpeechTranscriber.isAvailable ? 1 : 0
    }
    return 0
}

/// Decide whether this machine can transcribe `localeIdentifier` right now,
/// without recording or decoding anything.
///
/// Returns one of the `STATUS_*` codes. When `outDetail` is non-null it is set
/// to a `malloc`-owned C string the caller must release with
/// `slugtale_apple_speech_free`, or to null when there is nothing to add. The
/// detail never contains speech — see the status code comments for what each
/// one carries.
@_cdecl("slugtale_apple_speech_probe")
public func slugtale_apple_speech_probe(
    _ localeIdentifier: UnsafePointer<CChar>?,
    _ outDetail: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outDetail?.pointee = nil

    guard #available(macOS 26.0, *) else {
        outDetail?.pointee = copyToC(detectedOsVersion())
        return STATUS_UNSUPPORTED_OS
    }
    guard let requested = readString(localeIdentifier) else {
        outDetail?.pointee = copyToC("no Dictation Language was supplied")
        return STATUS_FAILED
    }

    let outcome = runBlocking { await probeLocale(requested) }
    outDetail?.pointee = outcome.detail.map(copyToC)
    return outcome.status
}

/// Transcribe one complete recording and return its final text.
///
/// `samples` is `sampleCount` mono 32-bit float samples at `sampleRateHz` — the
/// same Captured Audio the Whisper engine receives. The buffer is only read for
/// the duration of the call and is never copied anywhere but the audio buffers
/// handed to `SpeechAnalyzer`.
///
/// On `STATUS_OK`, `outText` receives the transcription and `outAlternatives`
/// receives the remaining whole-transcript alternatives joined by ASCII RS (or
/// null when the engine offered none). Both are `malloc`-owned and must be
/// released with `slugtale_apple_speech_free`. `outMeanConfidence` and
/// `outMinimumConfidence` receive scores in 0.0...1.0, or `-1.0` when
/// `SpeechTranscriber` reported no confidence for this recording.
///
/// On any other status `outText` and `outAlternatives` are null and `outDetail`
/// carries a non-content explanation.
@_cdecl("slugtale_apple_speech_transcribe")
public func slugtale_apple_speech_transcribe(
    _ samples: UnsafePointer<Float>?,
    _ sampleCount: Int,
    _ sampleRateHz: Double,
    _ localeIdentifier: UnsafePointer<CChar>?,
    _ outText: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ outAlternatives: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ outMeanConfidence: UnsafeMutablePointer<Double>?,
    _ outMinimumConfidence: UnsafeMutablePointer<Double>?,
    _ outDetail: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outText?.pointee = nil
    outAlternatives?.pointee = nil
    outDetail?.pointee = nil
    outMeanConfidence?.pointee = CONFIDENCE_UNREPORTED
    outMinimumConfidence?.pointee = CONFIDENCE_UNREPORTED

    guard #available(macOS 26.0, *) else {
        outDetail?.pointee = copyToC(detectedOsVersion())
        return STATUS_UNSUPPORTED_OS
    }
    guard let requested = readString(localeIdentifier) else {
        outDetail?.pointee = copyToC("no Dictation Language was supplied")
        return STATUS_FAILED
    }
    guard let samples, sampleCount > 0, sampleRateHz > 0 else {
        outDetail?.pointee = copyToC("the recording was empty")
        return STATUS_FAILED
    }

    // The samples belong to Rust for the whole call. Copying them into a Swift
    // array here — rather than capturing the raw pointer in an async closure
    // that outlives the frame — is what keeps that borrow honest.
    let owned = Array(UnsafeBufferPointer(start: samples, count: sampleCount))

    let outcome = runBlocking {
        await transcribeSamples(owned, sampleRateHz: sampleRateHz, localeIdentifier: requested)
    }

    outDetail?.pointee = outcome.detail.map(copyToC)
    guard outcome.status == STATUS_OK else { return outcome.status }

    outText?.pointee = copyToC(outcome.text)
    if !outcome.alternatives.isEmpty {
        outAlternatives?.pointee = copyToC(
            outcome.alternatives.joined(separator: ALTERNATIVE_SEPARATOR))
    }
    outMeanConfidence?.pointee = outcome.meanConfidence
    outMinimumConfidence?.pointee = outcome.minimumConfidence
    return STATUS_OK
}

/// Ask macOS to download and install the system speech assets for
/// `localeIdentifier`, blocking until it finishes.
///
/// Slugtale never calls this on its own: it is reachable only from an explicit
/// user action in Settings, because it spends the user's bandwidth and disk.
/// The assets stay owned by macOS — nothing here unpacks, copies, or caches
/// them, and Slugtale never redistributes them.
@_cdecl("slugtale_apple_speech_install_assets")
public func slugtale_apple_speech_install_assets(
    _ localeIdentifier: UnsafePointer<CChar>?,
    _ outDetail: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outDetail?.pointee = nil

    guard #available(macOS 26.0, *) else {
        outDetail?.pointee = copyToC(detectedOsVersion())
        return STATUS_UNSUPPORTED_OS
    }
    guard let requested = readString(localeIdentifier) else {
        outDetail?.pointee = copyToC("no Dictation Language was supplied")
        return STATUS_FAILED
    }

    let outcome = runBlocking { await installAssets(requested) }
    outDetail?.pointee = outcome.detail.map(copyToC)
    return outcome.status
}

/// Release a string this library returned. Every `outText`, `outAlternatives`,
/// and `outDetail` pointer comes from `strdup`, so exactly one `free` each;
/// passing null is a no-op so Rust can call it unconditionally.
@_cdecl("slugtale_apple_speech_free")
public func slugtale_apple_speech_free(_ pointer: UnsafeMutablePointer<CChar>?) {
    guard let pointer else { return }
    free(pointer)
}

// MARK: - Speech work

/// What one bridged call produced. Purely internal — it never crosses the C
/// boundary, so it can be an ordinary Swift struct.
private struct Outcome {
    var status: Int32
    var text: String = ""
    var alternatives: [String] = []
    var meanConfidence: Double = CONFIDENCE_UNREPORTED
    var minimumConfidence: Double = CONFIDENCE_UNREPORTED
    var detail: String?

    static func failure(_ status: Int32, _ detail: String?) -> Outcome {
        Outcome(status: status, detail: detail)
    }
}

/// Resolve `identifier` against the locales `SpeechTranscriber` can actually
/// transcribe. Apple matches loosely — asking for `en-GB` may legitimately land
/// on a broader English asset — so this goes through
/// `supportedLocale(equivalentTo:)` rather than comparing identifiers itself.
@available(macOS 26.0, *)
private func resolveLocale(_ identifier: String) async -> Locale? {
    await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: identifier))
}

/// The transcriber Slugtale asks for, in both the probe and the real run.
///
/// `.alternativeTranscriptions` and `.transcriptionConfidence` are requested
/// because the Second Opinion router is built on exactly those two signals; a
/// transcriber configured without them would look silent rather than confident.
/// `.volatileResults` is deliberately absent — Slugtale inserts one final
/// transcription and has no Live Preview (ADR-0005), so partial results would
/// be work with nowhere to go.
@available(macOS 26.0, *)
private func makeTranscriber(locale: Locale) -> SpeechTranscriber {
    SpeechTranscriber(
        locale: locale,
        transcriptionOptions: [],
        reportingOptions: [.alternativeTranscriptions],
        attributeOptions: [.transcriptionConfidence]
    )
}

@available(macOS 26.0, *)
private func probeLocale(_ identifier: String) async -> Outcome {
    guard SpeechTranscriber.isAvailable else {
        return .failure(
            STATUS_UNSUPPORTED_HARDWARE,
            "this Mac's hardware cannot run the on-device transcriber")
    }
    guard let locale = await resolveLocale(identifier) else {
        return .failure(STATUS_UNSUPPORTED_LOCALE, identifier)
    }

    switch await AssetInventory.status(forModules: [makeTranscriber(locale: locale)]) {
    case .installed:
        return Outcome(status: STATUS_OK)
    case .downloading:
        return .failure(
            STATUS_ASSETS_MISSING,
            "macOS is still downloading the speech assets for this language")
    case .supported:
        return .failure(
            STATUS_ASSETS_MISSING,
            "macOS has not installed the speech assets for this language yet")
    case .unsupported:
        return .failure(STATUS_UNSUPPORTED_LOCALE, identifier)
    @unknown default:
        // A future macOS could add a state. Treat it as "not ready" rather than
        // guessing, so the router falls back instead of transcribing blind.
        return .failure(
            STATUS_ASSETS_MISSING,
            "macOS reports an unrecognised state for this language's speech assets")
    }
}

@available(macOS 26.0, *)
private func installAssets(_ identifier: String) async -> Outcome {
    guard SpeechTranscriber.isAvailable else {
        return .failure(
            STATUS_UNSUPPORTED_HARDWARE,
            "this Mac's hardware cannot run the on-device transcriber")
    }
    guard let locale = await resolveLocale(identifier) else {
        return .failure(STATUS_UNSUPPORTED_LOCALE, identifier)
    }

    do {
        let modules = [makeTranscriber(locale: locale)]
        guard let request = try await AssetInventory.assetInstallationRequest(supporting: modules)
        else {
            // No request means macOS has nothing left to fetch.
            return Outcome(status: STATUS_OK)
        }
        try await request.downloadAndInstall()
        return Outcome(status: STATUS_OK)
    } catch {
        return .failure(STATUS_FAILED, describe(error))
    }
}

@available(macOS 26.0, *)
private func transcribeSamples(
    _ samples: [Float],
    sampleRateHz: Double,
    localeIdentifier: String
) async -> Outcome {
    let probe = await probeLocale(localeIdentifier)
    guard probe.status == STATUS_OK else { return probe }
    guard let locale = await resolveLocale(localeIdentifier) else {
        return .failure(STATUS_UNSUPPORTED_LOCALE, localeIdentifier)
    }

    let transcriber = makeTranscriber(locale: locale)

    // Ask the analyzer what it wants rather than assuming 16 kHz: the format it
    // names is the one that needs no resampling inside the engine, and feeding
    // anything else costs a conversion we would rather do once, here.
    guard
        let analyzerFormat = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [
            transcriber
        ])
    else {
        return .failure(STATUS_FAILED, "the transcriber offered no compatible audio format")
    }

    let buffer: AVAudioPCMBuffer
    do {
        buffer = try makeBuffer(samples, sampleRateHz: sampleRateHz, format: analyzerFormat)
    } catch {
        return .failure(STATUS_FAILED, describe(error))
    }

    let (inputStream, inputContinuation) = AsyncStream<AnalyzerInput>.makeStream()
    let analyzer = SpeechAnalyzer(modules: [transcriber])

    // Start draining results before feeding audio. `results` is a live sequence;
    // collecting it after the analyzer has already finalized would race with the
    // stream's completion and could drop the only result we care about.
    let collector = Task { () -> Result<Outcome, Error> in
        do {
            var collected = Outcome(status: STATUS_OK)
            var finalText = AttributedString()
            var alternatives: [String] = []
            for try await result in transcriber.results where result.isFinal {
                finalText.append(result.text)
                alternatives.append(
                    contentsOf: result.alternatives.map { String($0.characters) })
            }
            collected.text = String(finalText.characters)
            collected.alternatives = alternatives
            let (mean, minimum) = confidenceOf(finalText)
            collected.meanConfidence = mean
            collected.minimumConfidence = minimum
            return .success(collected)
        } catch {
            return .failure(error)
        }
    }

    do {
        inputContinuation.yield(AnalyzerInput(buffer: buffer))
        inputContinuation.finish()
        _ = try await analyzer.analyzeSequence(inputStream)
        try await analyzer.finalizeAndFinishThroughEndOfInput()
    } catch {
        collector.cancel()
        return .failure(STATUS_FAILED, describe(error))
    }

    switch await collector.value {
    case .success(let outcome):
        return outcome
    case .failure(let error):
        return .failure(STATUS_FAILED, describe(error))
    }
}

/// Wrap Slugtale's Captured Audio in the format the analyzer asked for,
/// converting only when the two differ.
@available(macOS 26.0, *)
private func makeBuffer(
    _ samples: [Float],
    sampleRateHz: Double,
    format: AVAudioFormat
) throws -> AVAudioPCMBuffer {
    guard
        let sourceFormat = AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: sampleRateHz,
            channels: 1,
            interleaved: false)
    else {
        throw BridgeError("the recording's audio format is not representable")
    }

    let frameCount = AVAudioFrameCount(samples.count)
    guard
        let source = AVAudioPCMBuffer(pcmFormat: sourceFormat, frameCapacity: frameCount),
        let channel = source.floatChannelData
    else {
        throw BridgeError("could not allocate an audio buffer for the recording")
    }
    source.frameLength = frameCount
    samples.withUnsafeBufferPointer { input in
        if let base = input.baseAddress {
            channel[0].update(from: base, count: samples.count)
        }
    }

    if sourceFormat.isEqual(format) {
        return source
    }

    guard let converter = AVAudioConverter(from: sourceFormat, to: format) else {
        throw BridgeError("could not convert the recording to the transcriber's audio format")
    }
    // Round up so the last partial frame of a resample survives, and add a
    // frame of slack for converters that emit a priming sample.
    let ratio = format.sampleRate / sourceFormat.sampleRate
    let capacity = AVAudioFrameCount((Double(samples.count) * ratio).rounded(.up)) + 1
    guard let converted = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: capacity) else {
        throw BridgeError("could not allocate a converted audio buffer")
    }

    var supplied = false
    var conversionError: NSError?
    let status = converter.convert(to: converted, error: &conversionError) { _, inputStatus in
        if supplied {
            inputStatus.pointee = .endOfStream
            return nil
        }
        supplied = true
        inputStatus.pointee = .haveData
        return source
    }
    if status == .error {
        throw conversionError ?? BridgeError("the recording could not be converted")
    }
    return converted
}

/// Reduce `SpeechTranscriber`'s per-run confidence attributes to the two numbers
/// the Transcription Engine boundary carries.
///
/// Runs are weighted by how many characters they cover: a one-word run and a
/// twelve-word run are not equally informative about the whole transcription.
/// The minimum stays unweighted — a single badly heard name is exactly the
/// signal the Second Opinion router escalates on, however short it is.
@available(macOS 26.0, *)
private func confidenceOf(_ text: AttributedString) -> (mean: Double, minimum: Double) {
    var weightedTotal = 0.0
    var weight = 0.0
    var minimum = Double.greatestFiniteMagnitude

    for run in text.runs {
        guard let confidence = run.transcriptionConfidence else { continue }
        let length = Double(text[run.range].characters.count)
        guard length > 0 else { continue }
        weightedTotal += confidence * length
        weight += length
        minimum = Swift.min(minimum, confidence)
    }

    guard weight > 0 else { return (CONFIDENCE_UNREPORTED, CONFIDENCE_UNREPORTED) }
    return (weightedTotal / weight, minimum)
}

// MARK: - Plumbing

private struct BridgeError: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

/// A non-content description of a failure. `localizedDescription` on Apple's
/// speech errors names the subsystem and the fault, never the audio, which is
/// what makes it safe to hand back to Rust and on to the Local Diagnostic Log.
private func describe(_ error: Error) -> String {
    if let bridge = error as? BridgeError { return bridge.description }
    return (error as NSError).localizedDescription
}

private func detectedOsVersion() -> String {
    let version = ProcessInfo.processInfo.operatingSystemVersion
    return "macOS \(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
}

private func readString(_ pointer: UnsafePointer<CChar>?) -> String? {
    guard let pointer else { return nil }
    return String(cString: pointer)
}

/// Copy a Swift string into a `malloc`-owned C string. Every string this library
/// hands out goes through here so there is exactly one allocator to free with.
private func copyToC(_ value: String) -> UnsafeMutablePointer<CChar> {
    strdup(value)!
}

/// Somewhere to park a value while a `DispatchSemaphore` bridges async Swift
/// back to the synchronous C ABI. The lock is not for contention — writer and
/// reader are separated by the semaphore — it is what makes the box `Sendable`
/// without an `nonisolated(unsafe)` escape hatch.
private final class Box<Value>: @unchecked Sendable {
    private let lock = NSLock()
    private var stored: Value?

    var value: Value? {
        get { lock.withLock { stored } }
        set { lock.withLock { stored = newValue } }
    }
}

/// Run an async operation to completion and return its result synchronously.
///
/// Rust calls this library from an ordinary thread — the dictation worker, never
/// a Swift cooperative-pool thread — so blocking it cannot deadlock the pool
/// that is executing the operation. Slugtale transcribes a whole recording at
/// once and has no Live Preview, so there is nothing to gain from an async Rust
/// signature and a callback ABI to match it.
private func runBlocking<Value>(_ operation: @escaping @Sendable () async -> Value) -> Value {
    let box = Box<Value>()
    let finished = DispatchSemaphore(value: 0)
    Task.detached(priority: .userInitiated) {
        box.value = await operation()
        finished.signal()
    }
    finished.wait()
    // Safe: the semaphore is only signalled after `value` has been written.
    return box.value!
}
