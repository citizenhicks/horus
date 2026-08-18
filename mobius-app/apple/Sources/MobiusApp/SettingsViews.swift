import SwiftUI

struct SettingsInfoButton: View {
    @Environment(\.mobiusPalette) private var palette
    @State private var showsDetail = false
    let title: String
    let detail: String
    var glyph: MobiusGlyph = .info
    var accessibilityHint = "Shows setting guidance"

    var body: some View {
        Button {
            showsDetail = true
        } label: {
            MobiusIcon(glyph, size: MobiusStyle.glyphInline, foreground: palette.muted)
                .frame(
                    minWidth: MobiusStyle.iconButtonSize,
                    minHeight: MobiusStyle.iconButtonSize
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.mobiusPlain)
        .accessibilityLabel("About \(title)")
        .accessibilityHint(accessibilityHint)
        .help("About \(title)")
        .sensoryFeedback(.selection, trigger: showsDetail)
        .popover(isPresented: $showsDetail) {
            VStack(alignment: .leading, spacing: MobiusSpace.s) {
                Text(title)
                    .font(MobiusStyle.controlFont.weight(.semibold))
                Text(detail)
                    .font(MobiusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(MobiusSpace.l)
            .frame(width: 280, alignment: .leading)
            .presentationCompactAdaptation(.popover)
        }
    }
}

struct SettingsStatusAccessory: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.mobiusPalette) private var palette
    @State private var showsStatus = false
    @Namespace private var namespace
    let subject: String
    let hasChanges: Bool
    let isSaving: Bool
    let saveDisabled: Bool
    let statusLabel: String
    let statusDetail: String
    let statusColor: Color
    let saveLabel: String
    var secondaryActionLabel: String?
    var secondaryAction: (() -> Void)?
    let save: () -> Void

    var body: some View {
        GlassEffectContainer(spacing: MobiusSpace.xxs) {
            HStack(spacing: MobiusSpace.xxs) {
                if hasChanges {
                    saveButton
                        .glassEffectID("\(subject)-save", in: namespace)
                }
                statusButton
                    .glassEffectID("\(subject)-status", in: namespace)
            }
        }
        .animation(
            reduceMotion ? nil : .spring(response: 0.34, dampingFraction: 0.78),
            value: hasChanges
        )
    }

    private var statusButton: some View {
        Button {
            showsStatus = true
        } label: {
            Label {
                Text("\(subject) status")
            } icon: {
                Circle()
                    .fill(statusColor)
                    .frame(width: 8, height: 8)
                    .symbolEffect(
                        .pulse.byLayer,
                        options: .repeat(.continuous),
                        isActive: !reduceMotion
                    )
            }
        }
        .mobiusIconButton()
        .accessibilityLabel("\(subject) status")
        .accessibilityValue(statusLabel)
        .help("\(subject): \(statusLabel)")
        .popover(isPresented: $showsStatus) {
            VStack(spacing: MobiusSpace.m) {
                Text(statusLabel)
                    .font(MobiusStyle.controlFont.weight(.semibold))
                    .foregroundStyle(statusColor)
                Text(statusDetail)
                    .font(MobiusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                if let secondaryActionLabel, let secondaryAction {
                    Divider()
                    Button(secondaryActionLabel) {
                        showsStatus = false
                        secondaryAction()
                    }
                }
            }
            .multilineTextAlignment(.center)
            .padding(MobiusSpace.l)
            .frame(width: 280)
            .presentationCompactAdaptation(.popover)
        }
    }

    private var saveButton: some View {
        Button(action: save) {
            Label {
                Text(saveLabel)
            } icon: {
                Group {
                    if isSaving {
                        MobiusSpinner(size: MobiusStyle.iconSize, foreground: palette.onAccent)
                    } else {
                        MobiusIcon(.saveAll, size: MobiusStyle.iconSize, foreground: palette.onAccent)
                    }
                }
            }
        }
        .mobiusProminentIconButton()
        .disabled(saveDisabled)
        .accessibilityLabel(saveLabel)
        .help(saveLabel)
        .sensoryFeedback(.success, trigger: hasChanges) { was, now in was && !now }
    }
}

struct GatewayView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    @State private var confirmsForget = false
    @State private var renameDraft = ""
    @State private var showsRename = false

    var body: some View {
        PageScaffold(
            title: "Gateway",
            detail: "Manage the selected gateway and pair another device."
        ) {
            Section {
                if model.accounts.isEmpty {
                    Text("No gateway configured on this device.")
                        .foregroundStyle(palette.muted)
                } else {
                    ForEach(model.accounts) { account in
                        LabeledContent(account.machineName) {
                            Text("Configured")
                                .foregroundStyle(palette.signal)
                        }
                    }
                }
            } header: {
                HStack(spacing: MobiusSpace.xs) {
                    Text("Configured")
                    SettingsInfoButton(
                        title: "Configured gateways",
                        detail: "Paired gateways are listed by the machine name they report after authentication."
                    )
                }
            }

            Section("Gateway") {
                Picker("Gateway", selection: Binding(
                    get: { model.selectedAccountID },
                    set: { model.selectAccount($0) }
                )) {
                    ForEach(model.accounts) { account in
                        Text(account.machineName)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .tag(Optional(account.id))
                    }
                }
                .settingsPickerStyle()
                .sensoryFeedback(.selection, trigger: model.selectedAccountID)
                LabeledContent("Status") {
                    HStack(spacing: MobiusSpace.s) {
                        Circle()
                            .fill(model.connectionState.isReady ? palette.signal : palette.danger)
                            .frame(width: 7, height: 7)
                        Text(model.connectionState.label)
                    }
                    .font(MobiusStyle.controlFont)
                }
                HStack(spacing: MobiusSpace.m) {
                    Text("Endpoint")
                    Spacer(minLength: MobiusSpace.s)
                    Text(model.selectedAccount?.endpoint.rawValue ?? "—")
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .textSelection(.enabled)
                }
                LabeledContent("Transport", value: transportName)
                LabeledContent("Wire protocol", value: "v\(gatewayProtocolVersion)")
            }

            HStack(spacing: MobiusSpace.s) {
                Button("Reconnect", glyph: .arrowClockwise, action: model.reconnect)
                    .mobiusIconButton()
                Button("Pair to self-hosted gateway", glyph: .plus) {
                    model.showsPairing = true
                }
                .mobiusIconButton()
                Button("Rename", glyph: .pencilSimple) {
                    renameDraft = model.selectedAccount?.displayName ?? ""
                    showsRename = true
                }
                .mobiusIconButton()
                .disabled(model.selectedAccount == nil)
                Button("Forget", glyph: .trash, role: .destructive) {
                    confirmsForget = true
                }
                .mobiusIconButton()
            }
            .frame(maxWidth: .infinity)
            .settingsStandaloneRow()

            Section("Pair another device") {
                SettingsCaption("Ask this gateway for a short-lived code, then enter it with the same gateway address on the other device.")
                if let pairing = model.pairingCodeInfo {
                    Text(pairing.code)
                        .font(.system(.title2, design: .monospaced, weight: .bold))
                        .tracking(3)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .center)
                    LabeledContent("Expires") {
                        Text(pairing.expiresAt, style: .relative)
                    }
                    .foregroundStyle(palette.muted)
                }
            }

            MobiusActionRow {
                if let pairing = model.pairingCodeInfo {
                    ShareLink("Copy or share", item: pairing.code)
                } else {
                    Button(
                        "Create one-time code",
                        glyph: .key,
                        action: model.createPairingCode
                    )
                        .mobiusProminentButton()
                }
            }
            .settingsStandaloneRow()

            Section("möbius Cloud") {
                SettingsCaption("Let möbius provision and manage a private gateway for you.")
                MobiusCloudOfferButton()
            }
        }
        .alert(
            "Forget this gateway?",
            isPresented: $confirmsForget
        ) {
            Button("Forget gateway", role: .destructive, action: model.forgetSelectedGateway)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("You will need to pair with this gateway again.")
        }
        .alert("Rename gateway", isPresented: $showsRename) {
            TextField("Gateway name", text: $renameDraft)
            Button("Cancel", role: .cancel) {}
            Button("Rename") { model.renameSelectedGateway(renameDraft) }
                .disabled(renameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    private var transportName: String {
        guard let endpoint = model.selectedAccount?.endpoint else { return "—" }
        if endpoint.usesWebSocket { return "WebSocket TLS" }
        return endpoint.usesTLS ? "TLS" : "Loopback TCP"
    }
}

struct PageScaffold<HeaderAccessory: View, Content: View>: View {
    @Environment(\.mobiusPalette) private var palette
    let title: String
    let detail: String
    let headerAccessory: HeaderAccessory
    let content: Content

    init(
        title: String,
        detail: String,
        @ViewBuilder headerAccessory: () -> HeaderAccessory,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.detail = detail
        self.headerAccessory = headerAccessory()
        self.content = content()
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                if !detail.isEmpty {
                    Text(detail)
                        .font(MobiusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                        .listRowBackground(Color.clear)
                        .listRowSeparator(.hidden)
                }
                content
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
            .scrollDismissesKeyboard(.interactively)
        }
        .navigationTitle(title)
        .toolbarTitleDisplayMode(.inline)
        .toolbar {
            // iOS 26 wraps a toolbar item in its own glass capsule. These accessories bring
            // their own treatment — the agent pages a pair of glass circles, the rest a bare
            // glyph — and the system's capsule drew a second background around both.
            ToolbarItem(placement: .topBarTrailing) { headerAccessory }
                .sharedBackgroundVisibility(.hidden)
        }
        .background(MobiusBackdrop())
    }
}

extension PageScaffold where HeaderAccessory == EmptyView {
    init(
        title: String,
        detail: String,
        @ViewBuilder content: () -> Content
    ) {
        self.init(
            title: title,
            detail: detail,
            headerAccessory: EmptyView.init,
            content: content
        )
    }
}

/// Secondary explanation under a form control.
private struct SettingsCaption: View {
    @Environment(\.mobiusPalette) private var palette
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(MobiusStyle.bodyFont)
            .foregroundStyle(palette.muted)
            .listRowSeparator(.hidden)
    }
}

extension View {
    /// A menu keeps the value on its own row without pushing a destination: the
    /// navigation-link style pushes a blank page from a split view's detail column.
    func settingsPickerStyle() -> some View {
        pickerStyle(.menu)
    }

    /// Trailing-aligned entry like Settings.app.
    func settingsField() -> some View {
        multilineTextAlignment(.trailing)
    }

    func settingsStandaloneRow() -> some View {
        Section {
            frame(maxWidth: .infinity)
                .listRowInsets(EdgeInsets(top: 6, leading: 0, bottom: 6, trailing: 0))
                .listRowBackground(Color.clear)
                .listRowSeparator(.hidden)
        }
    }
}

struct StatusBanner: View {
    enum Tone { case neutral, success, warning, error }
    @Environment(\.mobiusPalette) private var palette
    let tone: Tone
    let title: String
    let detail: String
    var progress = false
    var action: (String, @MainActor () -> Void)?

    var body: some View {
        HStack(spacing: MobiusSpace.m) {
            if progress { ProgressView().controlSize(.small) }
            else { MobiusIcon(glyph, foreground: color) }
            VStack(alignment: .leading, spacing: MobiusSpace.xs) {
                Text(title).font(MobiusStyle.controlFont)
                Text(detail).font(MobiusStyle.bodyFont).foregroundStyle(palette.muted)
            }
            Spacer()
            if let action {
                Button(action.0, action: action.1)
                    .buttonStyle(.mobiusGlass)
                    .buttonBorderShape(.capsule)
            }
        }
        .padding(MobiusSpace.m)
        .background(color.opacity(0.09), in: MobiusStyle.cardShape)
        .overlay {
            MobiusStyle.cardShape
                .stroke(color.opacity(0.45), lineWidth: MobiusStyle.borderWidth)
        }
    }

    private var color: Color {
        switch tone {
        case .neutral: palette.accent
        case .success: palette.signal
        case .warning: palette.warning
        case .error: palette.danger
        }
    }

    private var glyph: MobiusGlyph {
        switch tone {
        case .neutral: .info
        case .success: .sealCheck
        case .warning: .warning
        case .error: .warningOctagon
        }
    }
}

struct DisabledCapabilityNotice: View {
    let title: String
    let detail: String

    var body: some View {
        StatusBanner(tone: .neutral, title: title, detail: detail)
            .settingsStandaloneRow()
    }
}

func cacheHit(_ usage: TokenUsage) -> String {
    guard usage.inputTokens > 0 else { return "—" }
    return (Double(usage.cachedInputTokens) / Double(usage.inputTokens))
        .formatted(.percent.precision(.fractionLength(1)))
}
