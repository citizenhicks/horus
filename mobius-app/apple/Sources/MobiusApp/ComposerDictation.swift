@preconcurrency import AVFoundation
import Foundation
import Observation
import Speech

@MainActor
@Observable
final class ComposerDictation {
    enum State: Equatable {
        case idle
        case preparing
        case recording
        case stopping
    }

    private(set) var state = State.idle

    @ObservationIgnored private var audioEngine: AVAudioEngine?
    @ObservationIgnored private var audioContinuation: AsyncStream<AVAudioPCMBuffer>.Continuation?
    @ObservationIgnored private var inputContinuation: AsyncStream<AnalyzerInput>.Continuation?
    @ObservationIgnored private var analyzer: SpeechAnalyzer?
    @ObservationIgnored private var feedTask: Task<Void, Never>?
    @ObservationIgnored private var recognitionTask: Task<Void, Never>?
    @ObservationIgnored private var workerFailure: ComposerDictationError?
    @ObservationIgnored private var hasAudioTap = false
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var baseText = ""
    @ObservationIgnored private var separator = ""
    @ObservationIgnored private var finalizedText = ""
    @ObservationIgnored private var volatileText = ""
    @ObservationIgnored private var updateText: ((String) -> Void)?
    @ObservationIgnored private var reportError: ((String) -> Void)?

    var isActive: Bool { state != .idle }
    var isRecording: Bool { state == .recording }
    var isTransitioning: Bool { state == .preparing || state == .stopping }
    var canToggle: Bool { state == .idle || state == .recording }

    func start(
        existingText: String,
        updateText: @escaping (String) -> Void,
        reportError: @escaping (String) -> Void
    ) async throws {
        guard state == .idle else { return }
        state = .preparing
        generation += 1
        let currentGeneration = generation
        baseText = existingText
        separator = existingText.isEmpty || existingText.last?.isWhitespace == true ? "" : " "
        finalizedText = ""
        volatileText = ""
        self.updateText = updateText
        self.reportError = reportError
        workerFailure = nil

        do {
            guard await AVAudioApplication.requestRecordPermission() else {
                throw ComposerDictationError.microphoneDenied
            }
            try checkGeneration(currentGeneration)

            guard let locale = await DictationTranscriber.supportedLocale(
                equivalentTo: Locale.current
            ) else {
                throw ComposerDictationError.unsupportedLanguage
            }
            try checkGeneration(currentGeneration)

            let transcriber = DictationTranscriber(
                locale: locale,
                preset: .progressiveShortDictation
            )
            if let installation = try await AssetInventory.assetInstallationRequest(
                supporting: [transcriber]
            ) {
                try await installation.downloadAndInstall()
            }
            try checkGeneration(currentGeneration)

            guard let analyzerFormat = await SpeechAnalyzer.bestAvailableAudioFormat(
                compatibleWith: [transcriber]
            ) else {
                throw ComposerDictationError.audioUnavailable
            }
            try checkGeneration(currentGeneration)

            let analyzer = SpeechAnalyzer(modules: [transcriber])
            let (inputStream, inputContinuation) = AsyncStream<AnalyzerInput>.makeStream()
            self.analyzer = analyzer
            self.inputContinuation = inputContinuation
            let recognition = Task { [weak self] in
                do {
                    for try await result in transcriber.results {
                        guard !Task.isCancelled, let self else { return }
                        self.consume(result)
                    }
                } catch is CancellationError {
                    return
                } catch {
                    await self?.workerFailed(.transcriptionFailed)
                }
            }
            recognitionTask = recognition
            try await analyzer.start(inputSequence: inputStream)
            try checkGeneration(currentGeneration)

            let audioSession = AVAudioSession.sharedInstance()
            try audioSession.setCategory(.playAndRecord, mode: .spokenAudio)
            try audioSession.setActive(true, options: .notifyOthersOnDeactivation)

            let engine = AVAudioEngine()
            let inputNode = engine.inputNode
            let inputFormat = inputNode.outputFormat(forBus: 0)
            guard inputFormat.sampleRate > 0, inputFormat.channelCount > 0 else {
                throw ComposerDictationError.audioUnavailable
            }

            let (audioStream, audioContinuation) = AsyncStream<AVAudioPCMBuffer>.makeStream()
            self.audioContinuation = audioContinuation
            inputNode.installTap(
                onBus: 0,
                bufferSize: 4_096,
                format: inputFormat
            ) { buffer, _ in
                audioContinuation.yield(buffer)
            }
            hasAudioTap = true
            audioEngine = engine
            let feed = Task.detached(priority: .userInitiated) { [weak self] in
                let converter = ComposerAudioBufferConverter()
                defer { inputContinuation.finish() }
                do {
                    for await buffer in audioStream {
                        try Task.checkCancellation()
                        let converted = try converter.convert(buffer, to: analyzerFormat)
                        inputContinuation.yield(AnalyzerInput(buffer: converted))
                    }
                } catch is CancellationError {
                    return
                } catch {
                    await self?.workerFailed(.conversionFailed)
                }
            }
            feedTask = feed
            engine.prepare()
            try engine.start()
            try checkGeneration(currentGeneration)
            state = .recording
        } catch {
            let workerFailure = workerFailure
            await cancel()
            throw workerFailure ?? error
        }
    }

    func stop() async throws {
        guard state != .idle else { return }
        guard state == .recording else {
            await cancel()
            return
        }
        state = .stopping
        generation += 1
        audioEngine?.stop()
        removeAudioTap()
        audioContinuation?.finish()

        do {
            await feedTask?.value
            try checkWorkerFailure()
            inputContinuation?.finish()
            try await analyzer?.finalizeAndFinishThroughEndOfInput()
            await recognitionTask?.value
            try checkWorkerFailure()
            finish()
        } catch {
            await cancel()
            throw error
        }
    }

    func cancel() async {
        guard state != .idle else { return }
        state = .stopping
        generation += 1
        updateText?(renderedText(includeVolatile: false))
        updateText = nil
        audioEngine?.stop()
        removeAudioTap()
        audioContinuation?.finish()
        feedTask?.cancel()
        inputContinuation?.finish()
        await analyzer?.cancelAndFinishNow()
        recognitionTask?.cancel()
        finish()
    }

    private func consume(_ result: DictationTranscriber.Result) {
        let text = String(result.text.characters)
        if result.isFinal {
            finalizedText += text
            volatileText = ""
        } else {
            volatileText = text
        }
        updateText?(renderedText(includeVolatile: true))
    }

    private func workerFailed(_ failure: ComposerDictationError) async {
        guard state != .idle else { return }
        workerFailure = failure
        guard state != .stopping else { return }
        let reportError = reportError
        let wasPreparing = state == .preparing
        await cancel()
        if !wasPreparing {
            reportError?(failure.localizedDescription)
        }
    }

    private func renderedText(includeVolatile: Bool) -> String {
        let transcript = finalizedText + (includeVolatile ? volatileText : "")
        return transcript.isEmpty ? baseText : baseText + separator + transcript
    }

    private func checkGeneration(_ expected: Int) throws {
        guard generation == expected else { throw CancellationError() }
    }

    private func checkWorkerFailure() throws {
        if let workerFailure {
            throw workerFailure
        }
    }

    private func removeAudioTap() {
        guard hasAudioTap else { return }
        audioEngine?.inputNode.removeTap(onBus: 0)
        hasAudioTap = false
    }

    private func finish() {
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: .notifyOthersOnDeactivation
        )
        audioEngine = nil
        audioContinuation = nil
        inputContinuation = nil
        analyzer = nil
        feedTask = nil
        recognitionTask = nil
        hasAudioTap = false
        updateText = nil
        reportError = nil
        state = .idle
    }
}

private enum ComposerDictationError: LocalizedError {
    case microphoneDenied
    case unsupportedLanguage
    case audioUnavailable
    case conversionFailed
    case transcriptionFailed

    var errorDescription: String? {
        switch self {
        case .microphoneDenied:
            "Microphone access is required to dictate a message."
        case .unsupportedLanguage:
            "Dictation is not available for the current language."
        case .audioUnavailable:
            "The microphone is not available for dictation."
        case .conversionFailed:
            "möbius could not process the microphone audio."
        case .transcriptionFailed:
            "Dictation stopped unexpectedly. Please try again."
        }
    }
}

private final class ComposerAudioBufferConverter {
    private var converter: AVAudioConverter?

    func convert(_ buffer: AVAudioPCMBuffer, to format: AVAudioFormat) throws -> AVAudioPCMBuffer {
        guard buffer.format != format else { return buffer }
        if converter?.inputFormat != buffer.format || converter?.outputFormat != format {
            converter = AVAudioConverter(from: buffer.format, to: format)
            converter?.primeMethod = .none
        }
        guard let converter else { throw ComposerDictationError.conversionFailed }

        let ratio = converter.outputFormat.sampleRate / converter.inputFormat.sampleRate
        let capacity = max(
            1,
            AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up))
        )
        guard let converted = AVAudioPCMBuffer(
            pcmFormat: converter.outputFormat,
            frameCapacity: capacity
        ) else {
            throw ComposerDictationError.conversionFailed
        }

        var conversionError: NSError?
        // AVAudioConverter invokes this block synchronously; neither local escapes the call.
        nonisolated(unsafe) let input = buffer
        nonisolated(unsafe) var suppliedInput = false
        let status = converter.convert(to: converted, error: &conversionError) { _, status in
            guard !suppliedInput else {
                status.pointee = .noDataNow
                return nil
            }
            suppliedInput = true
            status.pointee = .haveData
            return input
        }
        guard status != .error else { throw ComposerDictationError.conversionFailed }
        return converted
    }
}
