import Foundation
#if canImport(FoundationModels)
import FoundationModels
#endif

/// Renames a new chat from its first prompt using Apple's on-device model.
///
/// The gateway names a chat after whatever the reader typed first, which is usually a
/// paragraph. The system model shortens it to a few words without the prompt leaving the
/// phone and without spending a gateway token on it. Every failure path — no Apple
/// Intelligence, a guardrail, an empty answer — leaves the gateway's own title alone.
@MainActor
final class ChatTitleWriter {
    typealias Generator = @MainActor @Sendable (String) async -> String?

    /// Long enough to stay specific, short enough for a sidebar row.
    nonisolated static let limit = 42
    /// The model only needs the shape of the request, not the whole essay.
    nonisolated private static let promptLimit = 600

    private let generator: Generator?

    init(generator: Generator? = nil) {
        self.generator = generator
    }

    var isAvailable: Bool {
        if generator != nil { return true }
        #if canImport(FoundationModels)
        return SystemLanguageModel.default.isAvailable
        #else
        return false
        #endif
    }

    func title(for prompt: String) async -> String? {
        if let generator { return await generator(prompt) }
        #if canImport(FoundationModels)
        guard isAvailable else { return nil }
        let session = LanguageModelSession {
            """
            You name chat threads in a coding assistant. Given the first message of a \
            thread, reply with a title of at most six words that says what the thread is \
            about. Reply with the title alone: no quotes, no punctuation at the end, no \
            explanation. Never answer the message itself.
            """
        }
        do {
            let response = try await session.respond(
                to: String(prompt.prefix(Self.promptLimit)),
                options: GenerationOptions(temperature: 0.3, maximumResponseTokens: 24)
            )
            return Self.cleaned(response.content)
        } catch {
            // A guardrail, an unloadable model, or a cancelled session. The chat keeps the
            // title it already has, which is never worse than before.
            return nil
        }
        #else
        return nil
        #endif
    }

    /// Small models like to wrap titles in quotes, prefix them with "Title:", and end them
    /// with a full stop. None of that belongs in a sidebar row.
    nonisolated static func cleaned(_ raw: String) -> String? {
        var title = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if let newline = title.firstIndex(of: "\n") {
            title = String(title[..<newline])
        }
        for prefix in ["Title:", "title:"] where title.hasPrefix(prefix) {
            title = String(title.dropFirst(prefix.count))
        }
        title = title.trimmingCharacters(in: CharacterSet(charactersIn: " \"'“”‘’`"))
        while let last = title.last, last == "." || last == "!" || last == "," {
            title = String(title.dropLast())
        }
        title = title.trimmingCharacters(in: .whitespaces)
        guard !title.isEmpty, title.count <= Self.limit else { return nil }
        return title
    }
}
