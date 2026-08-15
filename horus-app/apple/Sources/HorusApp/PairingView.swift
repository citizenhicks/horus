import SwiftUI

struct PairingView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horusPalette) private var palette
    let canCancel: Bool

    var body: some View {
        @Bindable var model = model
        ScrollView {
            VStack(alignment: .leading, spacing: HorusSpace.xl) {
                HStack(alignment: .top) {
                    SectionHeading(
                        title: "Pair with a gateway",
                        detail: "Use the same address and one-time code on iPad or iPhone."
                    )
                    Spacer()
                    if canCancel {
                        Button("Close", glyph: .x) {
                            model.showsPairing = false
                            dismiss()
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(HorusIconButtonStyle())
                        .help("Close")
                    }
                }

                VStack(spacing: HorusSpace.m) {
                    HorusCard {
                        VStack(alignment: .leading, spacing: HorusSpace.l) {
                            VStack(alignment: .leading, spacing: HorusSpace.s) {
                                Text("Gateway address")
                                    .font(HorusStyle.controlFont)
                                HStack {
                                    TextField("wss://gateway.example", text: $model.pairingEndpoint)
                                        .textFieldStyle(.roundedBorder)
                                        .textContentType(.URL)
                                        .autocorrectionDisabled()
                                        .controlSize(.large)
                                    PasteButton(payloadType: String.self) { values in
                                        if let value = values.first {
                                            model.applyPairingSetup(value)
                                        }
                                    }
                                    .labelStyle(.iconOnly)
                                    .buttonStyle(.glass)
                                    .buttonBorderShape(.circle)
                                    .controlSize(.large)
                                    .frame(
                                        width: HorusStyle.iconButtonSize,
                                        height: HorusStyle.iconButtonSize
                                    )
                                    .accessibilityLabel("Paste pairing setup")
                                    .help("Paste pairing setup")
                                }
                            }
                            VStack(alignment: .leading, spacing: HorusSpace.s) {
                                Text("One-time code")
                                    .font(HorusStyle.controlFont)
                                SecureField("One-time code", text: $model.pairingCode)
                                    .textFieldStyle(.roundedBorder)
                                    .controlSize(.large)
                            }
                        }
                    }

                    Text("Cloud gateways use wss://. tcp:// is accepted only for localhost; direct remote gateways can use tls://.")
                        .font(HorusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                        .multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, HorusSpace.s)

                    if let error = model.pairingError {
                        HorusLabel(
                            title: error,
                            glyph: .warning,
                            iconColor: palette.danger
                        )
                            .foregroundStyle(palette.danger)
                            .multilineTextAlignment(.center)
                    }
                }
                .frame(maxWidth: .infinity)
            }
        }
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize)
        .scrollDismissesKeyboard(.interactively)
        .safeAreaInset(edge: .bottom) { pairAction }
        .onSubmit { model.pair() }
    }

    /// The two ways in, then the wire detail. The protocol line led this stack before, which
    /// put the most technical line on the screen above the decision it belongs under.
    private var pairAction: some View {
        VStack(spacing: HorusSpace.m) {
            if model.connectionState == .connecting || model.connectionState == .authenticating {
                HStack {
                    HorusSpinner(size: HorusStyle.glyphLead, foreground: palette.accent)
                }
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(
                    model.connectionState == .authenticating
                        ? Text("Authenticating with gateway")
                        : Text("Connecting to gateway")
                )
            }
            Button("Pair to self-hosted gateway", action: model.pair)
                .horusProminentButton()
                .buttonBorderShape(.capsule)
                .controlSize(.large)
                .buttonSizing(.flexible)
            HorusCloudOfferButton()
            HorusLabel(
                title: "4-byte framed JSON · protocol v\(gatewayProtocolVersion)",
                glyph: .shieldCheck,
                iconColor: palette.muted
            )
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
                .padding(.top, HorusSpace.xxs)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, HorusSpace.l)
    }
}

extension String {
    var nonEmpty: String? {
        let trimmed = trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}
