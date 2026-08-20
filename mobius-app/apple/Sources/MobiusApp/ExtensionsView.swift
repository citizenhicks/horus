import SwiftUI

struct ExtensionsView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    @State private var isInstalling = false
    @State private var uninstalling: ExtensionRecord?

    var body: some View {
        let status = extensionStatus
        PageScaffold(
            title: "Extensions",
            detail: pageDetail,
            headerAccessory: {
                HStack(spacing: MobiusSpace.xxs) {
                    Button {
                        isInstalling = true
                    } label: {
                        MobiusIcon(.plus, gutter: false)
                    }
                    .mobiusProminentIconButton()
                    .disabled(!model.canMutateExtensions)
                    .accessibilityLabel("Install extension")
                    .accessibilityHint("Opens the extension installer")
                    .help("Install extension")
                    SettingsStatusAccessory(
                        subject: "Extensions",
                        hasChanges: false,
                        isSaving: model.extensionAction != nil,
                        saveDisabled: !model.canMutateExtensions,
                        statusLabel: status.label,
                        statusDetail: status.detail,
                        statusColor: status.color,
                        saveLabel: "Install extension",
                        save: { isInstalling = true }
                    )
                }
                .fixedSize()
            }
        ) {
            Section("Installed") {
                if model.extensions.isEmpty {
                    Text("Nothing installed yet.")
                        .font(MobiusStyle.captionFont)
                        .foregroundStyle(palette.muted)
                } else {
                    ForEach(model.extensions) { record in
                        installedRow(record)
                    }
                }
            }

            if !model.extensionSkillReferences.isEmpty {
                Section {
                    ForEach(model.extensionSkillReferences, id: \.value) { skill in
                        DiscoveredSkillRow(
                            name: skill.value,
                            description: skill.description
                        )
                    }
                } header: {
                    Text("Discovered")
                } footer: {
                    Text("Skills found in the gateway and workspace skill directories. They are always available and are not managed here.")
                }
            }
        }
        .sheet(isPresented: $isInstalling) { InstallExtensionSheet() }
        .alert(
            "Uninstall this extension?",
            isPresented: Binding(
                get: { uninstalling != nil },
                set: { if !$0 { uninstalling = nil } }
            )
        ) {
            Button("Uninstall", role: .destructive) {
                uninstalling.map(model.uninstallExtension)
                uninstalling = nil
            }
            Button("Cancel", role: .cancel) { uninstalling = nil }
        } message: {
            Text("The gateway will uninstall it without changing saved chat selections. Chats that reference it continue with the extension disabled. Per-workspace .mobius/extensions data is retained.")
        }
    }

    private var pageDetail: String {
        model.connectionState.isReady
            ? "Skills and plugins installed on the gateway from Git."
            : "Connect to a gateway to manage extensions."
    }

    private var extensionStatus: (label: String, detail: String, color: Color) {
        if let action = model.extensionAction {
            return switch action {
            case .installing:
                ("Installing extension", "The gateway is adding the package to its catalog.", palette.accent)
            case .updating(let name):
                ("Updating \(name)", "The gateway is replacing the installed snapshot.", palette.accent)
            case .uninstalling(let name):
                ("Uninstalling \(name)", "The gateway is removing the package from its catalog.", palette.accent)
            case .trusting(let name):
                ("Trusting \(name) hooks", "Trust is bound to the reviewed package digest.", palette.accent)
            case .untrusting(let name):
                ("Disabling \(name) hooks", "The gateway is revoking executable-hook trust.", palette.accent)
            }
        }
        switch model.connectionState {
        case .ready:
            return (
                "Catalog up to date",
                "\(model.extensions.count) installed · \(model.extensionSkillReferences.count) discovered",
                palette.signal
            )
        case .failed(let message):
            return ("Needs attention", message, palette.danger)
        default:
            return (
                model.connectionState.label,
                "Connect to a gateway to manage its extension catalog.",
                palette.warning
            )
        }
    }

    private func installedRow(_ record: ExtensionRecord) -> some View {
        HStack(spacing: MobiusSpace.s) {
            Button {
                model.navigationPath = [.settings(.extensionPackage(record.id))]
            } label: {
                InstalledExtensionLabel(record: record)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityHint("Shows extension details")

            if record.needsHookTrust {
                MobiusIcon(.shieldAlert, size: MobiusStyle.glyphMark, foreground: palette.warning)
                    .accessibilityLabel("\(record.name) has disabled hooks")
            }

            MobiusIcon(
                .caretRight,
                size: MobiusStyle.glyphMark,
                foreground: palette.muted
            )
            .accessibilityHidden(true)
        }
        .swipeActions(edge: .trailing) {
            Button {
                uninstalling = record
            } label: {
                MobiusIcon(.trash, foreground: palette.danger)
            }
            .tint(palette.panel)
            .disabled(!model.canMutateExtensions)
            .accessibilityLabel("Uninstall \(record.name)")
        }
        .swipeActions(edge: .leading) {
            Button {
                model.updateExtension(record)
            } label: {
                MobiusIcon(.arrowClockwise, foreground: palette.accent)
            }
            .tint(palette.panel)
            .disabled(!model.canMutateExtensions)
            .accessibilityLabel("Update \(record.name)")
        }
    }
}

// MARK: - Install

private struct InstallExtensionSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.mobiusPalette) private var palette

    var body: some View {
        @Bindable var model = model
        NavigationStack {
            Form {
                Section {
                    TextField("https://github.com/owner/repository.git", text: $model.extensionInstallSource)
                        .textContentType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.go)
                        .onSubmit(install)
                } header: {
                    Text("Git URL")
                } footer: {
                    Text("An HTTPS Git URL, or a GitHub tree URL pointing at a branch and subdirectory. The gateway clones it, pins an immutable snapshot, and reads its package manifest.")
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .navigationTitle("Install extension")
            .toolbarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel", role: .cancel) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Install", action: install).disabled(!canInstall)
                }
            }
            .background(MobiusBackdrop())
        }
        .presentationDragIndicator(.visible)
    }

    private var canInstall: Bool {
        model.canMutateExtensions
            && !model.extensionInstallSource.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    private func install() {
        guard canInstall else { return }
        model.installExtension()
        dismiss()
    }
}

// MARK: - Detail

struct ExtensionDetailView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var confirmsUninstall = false
    let id: String

    var body: some View {
        if let record = model.extensions.first(where: { $0.id == id }) {
            detail(record)
                .toolbarRole(.editor)
        } else {
            MobiusUnavailable(
                title: "Extension unavailable",
                glyph: .squaresFour,
                detail: "It is no longer installed on this gateway."
            )
            .navigationTitle("Extension")
            .toolbarRole(.editor)
            .background(MobiusBackdrop())
        }
    }

    private func detail(_ record: ExtensionRecord) -> some View {
        PageScaffold(
            title: record.name,
            detail: "",
            headerAccessory: {
                HStack(spacing: MobiusSpace.xxs) {
                    if !record.hooks.isEmpty {
                        if record.needsHookTrust {
                            Button {
                                model.trustHooks(for: record)
                            } label: {
                                MobiusIcon(.shieldCheck, gutter: false)
                            }
                            .mobiusIconButton()
                            .disabled(!model.canMutateExtensions)
                            .accessibilityLabel("Trust hooks")
                            .help("Trusts only the displayed package digest")
                        } else {
                            Button {
                                model.untrustHooks(for: record)
                            } label: {
                                MobiusIcon(.shieldOff, gutter: false)
                            }
                            .mobiusIconButton()
                            .disabled(!model.canMutateExtensions)
                            .accessibilityLabel("Untrust hooks")
                            .help("Untrust hooks")
                        }
                    }
                    Button {
                        model.updateExtension(record)
                    } label: {
                        MobiusIcon(.arrowClockwise, gutter: false)
                    }
                    .mobiusProminentIconButton()
                    .disabled(!model.canMutateExtensions)
                    .accessibilityLabel("Update extension")
                    .help("Update extension")
                    Button {
                        confirmsUninstall = true
                    } label: {
                        MobiusIcon(.trash, gutter: false)
                    }
                    .mobiusIconButton()
                    .disabled(!model.canMutateExtensions)
                    .accessibilityLabel("Uninstall extension")
                    .help("Uninstall extension")
                }
                .fixedSize()
            }
        ) {
            if !record.description.isEmpty {
                Section { Text(record.description) }
            }

            Section("Package") {
                LabeledContent("Kind", value: record.kind == .plugin ? "Plugin" : "Skill")
                if let version = record.version {
                    LabeledContent("Version") { Text(verbatim: version) }
                }
                LabeledContent("Source") { Text(verbatim: record.source) }
                if let reference = record.reference {
                    LabeledContent("Ref") { Text(verbatim: reference) }
                }
                if let subdirectory = record.subdirectory {
                    LabeledContent("Path") { Text(verbatim: subdirectory) }
                }
                MonospacedValue(label: "Revision", value: record.resolvedRevision)
                MonospacedValue(label: "Digest", value: record.digest)
            }

            if !record.skills.isEmpty {
                Section("Skills") {
                    ForEach(record.skills, id: \.self) { skill in
                        Text(verbatim: skill)
                    }
                }
            }

            if !record.hooks.isEmpty {
                ExtensionHooksSection(record: record)
            }

        }
        .alert(
            "Uninstall \(record.name)?",
            isPresented: $confirmsUninstall
        ) {
            Button("Uninstall", role: .destructive) {
                model.uninstallExtension(record)
                dismiss()
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The gateway will uninstall it without changing saved chat selections. Chats that reference it continue with the extension disabled. Per-workspace .mobius/extensions data is retained.")
        }
    }
}

private struct ExtensionHooksSection: View {
    @Environment(\.mobiusPalette) private var palette
    let record: ExtensionRecord

    var body: some View {
        Section {
            ForEach(identifiedHooks) { item in
                ExtensionHookRow(
                    hook: item.hook,
                    number: item.number,
                    count: record.hooks.count
                )
            }
        } header: {
            Text("Executable hooks")
        } footer: {
            Text(
                record.needsHookTrust
                    ? "These shell commands run on the gateway when their matching events fire. They stay disabled until you trust them. Trust applies only to the digest above, so an update disables them again."
                    : "Trusted for the digest above. An update to a different snapshot disables them until you trust it."
            )
            .foregroundStyle(record.needsHookTrust ? palette.warning : palette.muted)
        }
    }

    private var identifiedHooks: [IdentifiedExtensionHook] {
        var occurrences: [ExtensionHookRecord: Int] = [:]
        return record.hooks.enumerated().map { index, hook in
            let occurrence = occurrences[hook, default: 0]
            occurrences[hook] = occurrence + 1
            return IdentifiedExtensionHook(
                id: .init(hook: hook, occurrence: occurrence),
                hook: hook,
                number: index + 1
            )
        }
    }
}

private struct ExtensionHookRow: View {
    let hook: ExtensionHookRecord
    let number: Int
    let count: Int

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.s) {
            LabeledContent("Event") { Text(verbatim: shellSafe(hook.event)) }
            LabeledContent("Matcher") {
                Text(verbatim: hook.matcher.map(shellSafe) ?? "Any")
            }
            LabeledContent("Timeout", value: "\(hook.timeoutSeconds.formatted())s")
            MonospacedValue(label: "Command", value: shellSafe(hook.command))
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Hook \(number) of \(count)")
    }
}

private struct IdentifiedExtensionHook: Identifiable {
    struct ID: Hashable {
        let hook: ExtensionHookRecord
        let occurrence: Int
    }

    let id: ID
    let hook: ExtensionHookRecord
    let number: Int
}

// MARK: - Shared pieces

private struct InstalledExtensionLabel: View {
    @Environment(\.mobiusPalette) private var palette
    let record: ExtensionRecord

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
            Text(verbatim: record.name)
                .lineLimit(1)
            Text(verbatim: record.qualifiers)
                .font(MobiusStyle.captionFont)
                .foregroundStyle(palette.muted)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

private struct DiscoveredSkillRow: View {
    @Environment(\.mobiusPalette) private var palette
    let name: String
    let description: String

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
            Text(verbatim: name)
                .lineLimit(1)
            Text(verbatim: description)
                .font(MobiusStyle.captionFont)
                .foregroundStyle(palette.muted)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

/// A label over its value, where the value is machine text worth reading character by
/// character: a digest, a revision, a command.
private struct MonospacedValue: View {
    @Environment(\.mobiusPalette) private var palette
    let label: String
    let value: String

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.xs) {
            Text(label)
                .font(MobiusStyle.controlFont)
            Text(verbatim: value)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
        .accessibilityElement(children: .combine)
    }
}

/// Hook text is attacker-controlled: it arrives from a cloned repository and is shown so an
/// owner can decide whether to let it run. `debugDescription` quotes it and escapes control
/// characters, so a command cannot spoof surrounding UI or hide behind a newline.
private func shellSafe(_ value: String) -> String {
    String(reflecting: value)
}

private extension ExtensionRecord {
    var needsHookTrust: Bool { !hooks.isEmpty && !hooksTrusted }

    var qualifiers: String {
        var parts = [kind == .plugin ? "Plugin" : "Skill"]
        if let version { parts.append(version) }
        return parts.joined(separator: " · ")
    }
}
