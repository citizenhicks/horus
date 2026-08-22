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
        SettingsStatusButton(
            subject: subject,
            statusLabel: statusLabel,
            statusDetail: statusDetail,
            statusColor: statusColor,
            secondaryActionLabel: secondaryActionLabel,
            secondaryAction: secondaryAction
        )
        .mobiusIconButton()
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

struct SettingsStatusButton: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.mobiusPalette) private var palette
    @State private var showsStatus = false
    let subject: String
    let statusLabel: String
    let statusDetail: String
    let statusColor: Color
    var secondaryActionLabel: String?
    var secondaryAction: (() -> Void)?

    var body: some View {
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
}

struct GatewayView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    @State private var forgetting: GatewayAccount?

    var body: some View {
        let status = gatewayStatus
        PageScaffold(
            title: "Gateway",
            detail: "Gateways this device is paired with. Chats run on the selected one.",
            sharesHeaderBackground: true,
            headerAccessory: {
                HeaderActionGroup {
                    Button {
                        model.showsPairing = true
                    } label: {
                        MobiusIcon(.plus, gutter: false)
                    }
                    .groupedHeaderAction(prominent: true)
                    .accessibilityLabel("Pair gateway")
                    .accessibilityHint("Opens pairing with a self-hosted gateway")
                    .help("Pair gateway")
                    SettingsStatusButton(
                        subject: "Gateway",
                        statusLabel: status.label,
                        statusDetail: status.detail,
                        statusColor: status.color
                    )
                    .labelStyle(.iconOnly)
                    .groupedHeaderAction()
                }
            }
        ) {
            if !model.accounts.isEmpty {
                Section("Active") {
                    // The same control as the chats header, so switching does not
                    // require stepping into a gateway's detail page.
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
                                .fill(model.connectionState.tone.color(in: palette))
                                .frame(width: 7, height: 7)
                            Text(model.connectionState.label)
                        }
                        .font(MobiusStyle.controlFont)
                    }
                }
            }

            Section("Paired") {
                if model.accounts.isEmpty {
                    Text("No gateway paired on this device.")
                        .font(MobiusStyle.captionFont)
                        .foregroundStyle(palette.muted)
                } else {
                    ForEach(model.accounts) { account in
                        pairedRow(account)
                    }
                }
            }

            if !model.hasCloudAccount || model.cloudAccount?.subscribed == false {
                Section("möbius Cloud") {
                    SettingsCaption("Let möbius provision and manage a private gateway for you.")
                    MobiusCloudOfferButton()
                }
            }
        }
        .alert(
            "Forget this gateway?",
            isPresented: Binding(
                get: { forgetting != nil },
                set: { if !$0 { forgetting = nil } }
            )
        ) {
            Button("Forget gateway", role: .destructive) {
                forgetting.map(model.forgetGateway)
                forgetting = nil
            }
            Button("Cancel", role: .cancel) { forgetting = nil }
        } message: {
            Text("You will need to pair with this gateway again.")
        }
    }

    private var gatewayStatus: (label: String, detail: String, color: Color) {
        switch model.connectionState {
        case .ready:
            return (
                model.connectionState.label,
                "\(model.accounts.count) paired · \(model.selectedAccount?.machineName ?? "none") selected",
                palette.signal
            )
        case .failed(let message):
            return ("Needs attention", message, palette.danger)
        default:
            return (
                model.connectionState.label,
                "Pair a gateway to run chats on it.",
                palette.warning
            )
        }
    }

    private func pairedRow(_ account: GatewayAccount) -> some View {
        HStack(spacing: MobiusSpace.s) {
            Button {
                model.navigationPath = [.settings(.gateway(account.id))]
            } label: {
                PairedGatewayLabel(account: account)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityHint("Shows this gateway's settings")

            if account.id == model.selectedAccountID {
                MobiusIcon(.check, size: MobiusStyle.glyphMark, foreground: palette.signal)
                    .accessibilityLabel("Selected")
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
                forgetting = account
            } label: {
                MobiusIcon(.trash, foreground: palette.danger)
            }
            .tint(palette.panel)
            .accessibilityLabel("Forget \(account.machineName)")
        }
    }
}

private struct PairedGatewayLabel: View {
    let account: GatewayAccount

    var body: some View {
        Text(verbatim: account.machineName)
            .lineLimit(1)
            .truncationMode(.middle)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
    }
}

private let githubCredentialTarget = "https://github.com"

struct GatewayDetailView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    @Environment(\.dismiss) private var dismiss
    @State private var confirmsForget = false
    @State private var showsRename = false
    @State private var showsGitCredential = false
    @State private var renameDraft = ""
    let id: UUID

    var body: some View {
        @Bindable var model = model
        if let account = model.accounts.first(where: { $0.id == id }) {
            detail(account)
                .toolbarRole(.editor)
                .alert("Forget this gateway?", isPresented: $confirmsForget) {
                    Button("Forget gateway", role: .destructive) {
                        model.forgetGateway(account)
                        dismiss()
                    }
                    Button("Cancel", role: .cancel) {}
                } message: {
                    Text("You will need to pair with this gateway again.")
                }
                .alert("Rename gateway", isPresented: $showsRename) {
                    TextField("Gateway name", text: $renameDraft)
                    Button("Cancel", role: .cancel) {}
                    Button("Rename") { model.renameGateway(account, to: renameDraft) }
                        .disabled(
                            renameDraft
                                .trimmingCharacters(in: .whitespacesAndNewlines)
                                .isEmpty
                        )
                }
                .sheet(isPresented: $showsGitCredential) {
                    GitCredentialSheet()
                }
                .sheet(
                    item: $model.generatedSshIdentity,
                    content: SshPublicKeySheet.init
                )
                .task(id: model.connectionState.isReady) {
                    guard account.id == model.selectedAccountID,
                          model.connectionState.isReady
                    else { return }
                    if model.gitCredentialAvailable == nil {
                        model.probeGitCredential(githubCredentialTarget)
                    }
                    if model.sshIdentities == nil {
                        model.listSshIdentities()
                    }
                }
        } else {
            MobiusUnavailable(
                title: "Gateway unavailable",
                glyph: AppDestination.gateway.glyph,
                detail: "It is no longer paired on this device."
            )
            .navigationTitle("Gateway")
            .toolbarRole(.editor)
            .background(MobiusBackdrop())
        }
    }

    private func detail(_ account: GatewayAccount) -> some View {
        let isActive = account.id == model.selectedAccountID
        return PageScaffold(
            title: account.displayName,
            detail: "",
            sharesHeaderBackground: true,
            headerAccessory: {
                HeaderOptionsMenu(label: "Gateway actions") {
                    if isActive {
                        Button(action: model.reconnect) {
                            MobiusLabel(title: "Reconnect", glyph: .arrowClockwise)
                        }
                    }
                    Button {
                        renameDraft = account.displayName
                        showsRename = true
                    } label: {
                        MobiusLabel(title: "Rename gateway", glyph: .pencilSimple)
                    }
                    Button(role: .destructive) {
                        confirmsForget = true
                    } label: {
                        MobiusLabel(title: "Forget gateway", glyph: .trash)
                    }
                }
            }
        ) {
            Section("Connection") {
                if isActive {
                    LabeledContent("Status") {
                        HStack(spacing: MobiusSpace.s) {
                            Circle()
                                .fill(model.connectionState.tone.color(in: palette))
                                .frame(width: 7, height: 7)
                            Text(model.connectionState.label)
                        }
                        .font(MobiusStyle.controlFont)
                    }
                }
                HStack(spacing: MobiusSpace.m) {
                    Text("Endpoint")
                    Spacer(minLength: MobiusSpace.s)
                    Text(account.endpoint.rawValue)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .trailing)
                        .textSelection(.enabled)
                }
                LabeledContent("Transport", value: transportName(account))
                LabeledContent("Machine", value: account.machineName)
                LabeledContent("Wire protocol", value: "v\(gatewayProtocolVersion)")
            }

            if isActive {
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

                Section("Host credentials") {
                    if model.gitCredentialAvailable == false {
                        Button {
                            showsGitCredential = true
                        } label: {
                            gitCredentialRow
                        }
                        .buttonStyle(.plain)
                        .disabled(!model.connectionState.isReady)
                        .accessibilityLabel("Set up GitHub credentials")
                        .accessibilityValue(gitCredentialSummary)
                        .accessibilityHint("Adds a GitHub HTTPS credential to this gateway host")
                    } else {
                        gitCredentialRow
                            .accessibilityElement(children: .combine)
                            .accessibilityLabel("GitHub credentials")
                            .accessibilityValue(gitCredentialSummary)
                    }

                    sshCredentialRow
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("SSH identities")
                        .accessibilityValue(sshCredentialSummary)

                    if let identities = model.sshIdentities {
                        ForEach(identities) { identity in
                            VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
                                LabeledContent {
                                    Text(verbatim: identity.algorithm)
                                        .foregroundStyle(palette.muted)
                                } label: {
                                    Text(verbatim: identity.label)
                                }
                                Text(verbatim: identity.fingerprint)
                                    .font(.system(.caption, design: .monospaced))
                                    .foregroundStyle(palette.muted)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                                    .textSelection(.enabled)
                            }
                            .accessibilityElement(children: .combine)
                            .accessibilityLabel(identity.label)
                            .accessibilityValue(
                                "\(identity.algorithm), \(identity.fingerprint)"
                            )
                        }
                    }
                }

                if model.sshIdentities?.isEmpty == true {
                    MobiusActionRow {
                        Button(
                            model.isGeneratingSshIdentity
                                ? "Generating on host…"
                                : "Generate SSH key on host",
                            glyph: .key,
                            action: model.generateSshIdentity
                        )
                        .mobiusProminentButton()
                        .disabled(
                            model.isGeneratingSshIdentity
                                || !model.connectionState.isReady
                        )
                    }
                    .settingsStandaloneRow()
                }
            }
        }
    }

    private var gitCredentialRow: some View {
        HStack(spacing: MobiusSpace.m) {
            MobiusIcon(.gitBranch, size: MobiusStyle.glyphInline)
            VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
                Text("GitHub")
                    .font(MobiusStyle.controlFont)
                Text(gitCredentialSummary)
                    .font(MobiusStyle.captionFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(2)
            }
            Spacer(minLength: MobiusSpace.s)
            if model.isCheckingGitCredential {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityHidden(true)
            } else {
                MobiusIcon(
                    model.gitCredentialAvailable == true ? .checkCircle : .caretRight,
                    size: MobiusStyle.glyphMark,
                    foreground: model.gitCredentialAvailable == true
                        ? palette.signal
                        : palette.muted
                )
                .accessibilityHidden(true)
            }
        }
        .contentShape(Rectangle())
    }

    private var gitCredentialSummary: String {
        if !model.connectionState.isReady { return "Connect to check this host." }
        if model.isCheckingGitCredential { return "Checking this host…" }
        if model.gitCredentialAvailable == true { return "Credential found on this host." }
        if model.gitCredentialAvailable == false { return "No credential found. Set up GitHub." }
        return "Couldn’t check this host."
    }

    private var sshCredentialRow: some View {
        HStack(spacing: MobiusSpace.m) {
            MobiusIcon(.fingerprint, size: MobiusStyle.glyphInline)
            VStack(alignment: .leading, spacing: MobiusSpace.xxs) {
                Text("SSH")
                    .font(MobiusStyle.controlFont)
                Text(sshCredentialSummary)
                    .font(MobiusStyle.captionFont)
                    .foregroundStyle(palette.muted)
                    .lineLimit(2)
            }
            Spacer(minLength: MobiusSpace.s)
            if model.isLoadingSshIdentities || model.isGeneratingSshIdentity {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityHidden(true)
            } else if model.sshIdentities?.isEmpty == false {
                MobiusIcon(
                    .checkCircle,
                    size: MobiusStyle.glyphMark,
                    foreground: palette.signal
                )
                .accessibilityHidden(true)
            }
        }
    }

    private var sshCredentialSummary: String {
        if !model.connectionState.isReady { return "Connect to check this host." }
        if model.isLoadingSshIdentities { return "Checking this host…" }
        if model.isGeneratingSshIdentity { return "Generating an Ed25519 key on this host…" }
        if let error = model.sshIdentityError { return error }
        guard let identities = model.sshIdentities else { return "Couldn’t check this host." }
        if identities.isEmpty { return "No public identities found." }
        return "\(identities.count) public \(identities.count == 1 ? "identity" : "identities") found."
    }

    private func transportName(_ account: GatewayAccount) -> String {
        if account.endpoint.usesWebSocket { return "WebSocket TLS" }
        return account.endpoint.usesTLS ? "TLS" : "Loopback TCP"
    }
}

private struct GitCredentialSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.mobiusPalette) private var palette
    @State private var username = ""
    @State private var token = ""

    var body: some View {
        NavigationStack {
            Form {
                Section("GitHub") {
                    LabeledContent("Host", value: "github.com")
                }

                if model.gitCredentialAvailable != true {
                    Section {
                        TextField("GitHub username", text: $username)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                        SecureField("Personal access token", text: $token)
                            .textContentType(.password)
                            .privacySensitive()
                    } header: {
                        Text("Credential")
                    } footer: {
                        Text("Sent once to the host's configured Git helper. Möbius does not store or read it back.")
                    }
                }

                if let error = model.gitCredentialError {
                    Text(error)
                        .font(MobiusStyle.captionFont)
                        .foregroundStyle(palette.danger)
                }
            }
            .navigationTitle("GitHub credentials")
            .toolbarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    actionButton
                }
            }
        }
        .presentationDetents([.medium, .large])
    }

    @ViewBuilder
    private var actionButton: some View {
        if model.gitCredentialAvailable == true {
            Button("Done") { dismiss() }
        } else {
            Button(model.isCheckingGitCredential ? "Saving…" : "Save") {
                let value = token
                token = ""
                model.approveGitCredential(
                    target: githubCredentialTarget,
                    username: username,
                    token: value
                )
            }
            .disabled(
                model.isCheckingGitCredential
                    || username.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                    || token.isEmpty
            )
        }
    }
}

private struct SshPublicKeySheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.mobiusPalette) private var palette
    let result: GeneratedSshIdentity

    var body: some View {
        NavigationStack {
            Form {
                Section("Created on host") {
                    LabeledContent("Label", value: result.identity.label)
                    LabeledContent("Algorithm", value: result.identity.algorithm)
                    LabeledContent("Fingerprint") {
                        Text(verbatim: result.identity.fingerprint)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                    }
                }

                Section {
                    Text(verbatim: result.publicKey)
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(palette.muted)
                        .fixedSize(horizontal: false, vertical: true)
                        .textSelection(.enabled)
                } header: {
                    Text("Public key")
                } footer: {
                    Text("Add this public key to GitHub or another remote. Creating it does not grant access by itself. The private key stays on the gateway host.")
                }

                MobiusActionRow {
                    ShareLink("Copy or share", item: result.publicKey)
                }
                .settingsStandaloneRow()
            }
            .navigationTitle("SSH key created")
            .toolbarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done", action: dismiss.callAsFunction)
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

struct PageScaffold<HeaderAccessory: View, Content: View>: View {
    @Environment(\.mobiusPalette) private var palette
    let title: String
    let detail: String
    let sharesHeaderBackground: Bool
    let headerAccessory: HeaderAccessory
    let content: Content

    init(
        title: String,
        detail: String,
        sharesHeaderBackground: Bool = false,
        @ViewBuilder headerAccessory: () -> HeaderAccessory,
        @ViewBuilder content: () -> Content
    ) {
        self.title = title
        self.detail = detail
        self.sharesHeaderBackground = sharesHeaderBackground
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
            if sharesHeaderBackground {
                ToolbarItem(placement: .primaryAction) { headerAccessory }
            } else {
                ToolbarItem(placement: .primaryAction) { headerAccessory }
                    .sharedBackgroundVisibility(.hidden)
            }
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
            sharesHeaderBackground: false,
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
