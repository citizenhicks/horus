import SwiftUI

struct SettingsInfoButton: View {
    @Environment(\.horusPalette) private var palette
    @State private var showsDetail = false
    let title: String
    let detail: String
    var glyph: HorusGlyph = .info
    var accessibilityHint = "Shows setting guidance"

    var body: some View {
        Button {
            showsDetail = true
        } label: {
            HorusIcon(glyph, size: HorusStyle.glyphInline, foreground: palette.muted)
                .frame(
                    minWidth: HorusStyle.iconButtonSize,
                    minHeight: HorusStyle.iconButtonSize
                )
                .contentShape(Rectangle())
        }
        .buttonStyle(.horusPlain)
        .accessibilityLabel("About \(title)")
        .accessibilityHint(accessibilityHint)
        .help("About \(title)")
        .sensoryFeedback(.selection, trigger: showsDetail)
        .popover(isPresented: $showsDetail) {
            VStack(alignment: .leading, spacing: HorusSpace.s) {
                Text(title)
                    .font(HorusStyle.controlFont.weight(.semibold))
                Text(detail)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(HorusSpace.l)
            .frame(width: 280, alignment: .leading)
            .presentationCompactAdaptation(.popover)
        }
    }
}

struct GatewayView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
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
                HStack(spacing: HorusSpace.xs) {
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
                    HStack(spacing: HorusSpace.s) {
                        Circle()
                            .fill(model.connectionState.isReady ? palette.signal : palette.danger)
                            .frame(width: 7, height: 7)
                        Text(model.connectionState.label)
                    }
                    .font(HorusStyle.controlFont)
                }
                HStack(spacing: HorusSpace.m) {
                    Text("Endpoint")
                    Spacer(minLength: HorusSpace.s)
                    Text(model.selectedAccount?.endpoint.rawValue ?? "—")
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .textSelection(.enabled)
                }
                LabeledContent("Transport", value: transportName)
                LabeledContent("Wire protocol", value: "v\(gatewayProtocolVersion)")
            }

            HorusActionRow(collapsesToIcons: true) {
                Button("Reconnect", glyph: .arrowClockwise, action: model.reconnect)
                Button("Pair to self-hosted gateway", glyph: .plus) {
                    model.showsPairing = true
                }
                Button("Rename", glyph: .pencilSimple) {
                    renameDraft = model.selectedAccount?.displayName ?? ""
                    showsRename = true
                }
                .disabled(model.selectedAccount == nil)
                Button("Forget", glyph: .trash, role: .destructive) {
                    confirmsForget = true
                }
            }
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

            HorusActionRow {
                if let pairing = model.pairingCodeInfo {
                    ShareLink("Copy or share", item: pairing.code)
                } else {
                    Button(
                        "Create one-time code",
                        glyph: .key,
                        action: model.createPairingCode
                    )
                        .horusProminentButton()
                }
            }
            .settingsStandaloneRow()

            Section("Horus Cloud") {
                SettingsCaption("Let Horus provision and manage a private gateway for you, with a 7-day trial and included Luna usage.")
                HorusCloudOfferButton()
            }
        }
        .confirmationDialog(
            "Forget this gateway?",
            isPresented: $confirmsForget,
            titleVisibility: .visible
        ) {
            Button("Forget gateway", role: .destructive, action: model.forgetSelectedGateway)
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
    @Environment(\.horusPalette) private var palette
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
                        .font(HorusStyle.bodyFont)
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
        .background(HorusBackdrop())
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
    @Environment(\.horusPalette) private var palette
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text)
            .font(HorusStyle.bodyFont)
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
    @Environment(\.horusPalette) private var palette
    let tone: Tone
    let title: String
    let detail: String
    var progress = false
    var action: (String, @MainActor () -> Void)?

    var body: some View {
        HStack(spacing: HorusSpace.m) {
            if progress { ProgressView().controlSize(.small) }
            else { HorusIcon(glyph, foreground: color) }
            VStack(alignment: .leading, spacing: HorusSpace.xs) {
                Text(title).font(HorusStyle.controlFont)
                Text(detail).font(HorusStyle.bodyFont).foregroundStyle(palette.muted)
            }
            Spacer()
            if let action {
                Button(action.0, action: action.1)
                    .buttonStyle(.horusGlass)
                    .buttonBorderShape(.capsule)
            }
        }
        .padding(HorusSpace.m)
        .background(color.opacity(0.09), in: HorusStyle.cardShape)
        .overlay {
            HorusStyle.cardShape
                .stroke(color.opacity(0.45), lineWidth: HorusStyle.borderWidth)
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

    private var glyph: HorusGlyph {
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
