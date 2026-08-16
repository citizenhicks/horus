import AuthenticationServices
import StoreKit
import SwiftUI

struct MobiusCloudOfferButton: View {
    @Environment(\.mobiusPalette) private var palette
    @State private var product: Product?
    @State private var showsOffer = false

    // A centred label and no chevron: with a leading glyph, a spacer and a caret this read
    // as a list row that happened to be capsule-shaped. The accent tint marks it as the
    // other path rather than a second copy of the pairing button.
    var body: some View {
        Button {
            showsOffer = true
        } label: {
            Label {
                title
                    // Glass takes a tint from its own material, not from the button's, so
                    // the accent has to be carried by the label for it to read at all.
                    .foregroundStyle(palette.accent)
            } icon: {
                // The product's own mark, drawn full-colour: the logo is artwork rather
                // than a template glyph, so it keeps its own colours beside accent text.
                Image("MobiusLogo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 20, height: 20)
                    .accessibilityHidden(true)
            }
            .font(MobiusStyle.controlFont)
        }
        .buttonStyle(.mobiusGlass)
        .tint(palette.accent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .buttonSizing(.flexible)
        .task { await loadProduct() }
        .sheet(isPresented: $showsOffer) {
            MobiusCloudOfferSheet()
                .presentationDragIndicator(.visible)
        }
        .accessibilityHint("Explains the managed möbius Cloud subscription")
    }

    private var title: Text {
        guard let product else { return Text("Connect to möbius Cloud") }
        return Text("Connect to möbius Cloud for \(product.displayPrice) per month")
    }

    @MainActor
    private func loadProduct() async {
        guard product == nil else { return }
        product = try? await Product.products(for: [mobiusCloudMonthlyProductID]).first
    }
}

private struct MobiusCloudOfferSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.mobiusPalette) private var palette
    @State private var appleNonce: MobiusCloudAppleNonce?
    @State private var product: Product?
    @State private var productLoadFailed = false

    var body: some View {
        NavigationStack {
            ZStack {
                MobiusBackdrop()
                ScrollView {
                    VStack(alignment: .leading, spacing: MobiusSpace.xl) {
                        hero
                        offerDetails
                        controlNote
                    }
                    .frame(maxWidth: 680, alignment: .leading)
                    .padding(.horizontal, MobiusSpace.l)
                    .padding(.top, MobiusSpace.l)
                    .padding(.bottom, MobiusSpace.xl)
                    .frame(maxWidth: .infinity)
                }
                .scrollIndicators(.hidden)
            }
            .navigationTitle("möbius Cloud")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .safeAreaInset(edge: .bottom) { signupBoundary }
        }
        .interactiveDismissDisabled(model.cloudAction.isRunning)
        .task { await loadProduct() }
    }

    /// One voice per line: a mark, the promise, and the shape of the offer. The price is not
    /// here — it belongs beside the button that charges it, not at the top in accent.
    private var hero: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.l) {
            // The app's own mark, not a stock globe.
            MobiusComposingOrb()
                .frame(width: 64, height: 64)
                .frame(maxWidth: .infinity)
                .accessibilityHidden(true)
            Text("Your private gateway, managed by möbius.")
                .font(.largeTitle.weight(.bold))
                .fixedSize(horizontal: false, vertical: true)
            Text(
                "Skip server setup without giving up control. We provision, secure, and maintain a gateway scoped to your account."
            )
                .font(MobiusStyle.bodyFont)
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var offerDetails: some View {
        MobiusCard {
            VStack(alignment: .leading, spacing: 0) {
                CloudBenefit(
                    glyph: .sparkle,
                    title: "The open-source gateway, hosted for you",
                    detail: "Run the same generic möbius gateway in a private, persistent workspace."
                )
                Divider().padding(.leading, MobiusStyle.glyphGutter + MobiusSpace.m)
                CloudBenefit(
                    glyph: .setup01,
                    title: "Fast, modular harness",
                    detail: "Choose the providers, tools, and capabilities you want while möbius keeps the runtime lean."
                )
                Divider().padding(.leading, MobiusStyle.glyphGutter + MobiusSpace.m)
                CloudBenefit(
                    glyph: .key,
                    title: "Bring your own keys",
                    detail: "Connect your own model provider account without storing its API key in möbius Cloud or the gateway filesystem."
                )
                Divider().padding(.leading, MobiusStyle.glyphGutter + MobiusSpace.m)
                CloudBenefit(
                    glyph: .shieldCheck,
                    title: "Encrypted and user-scoped",
                    detail: "Your gateway, credentials, and cloud data stay isolated to your account."
                )
            }
        }
    }

    private var controlNote: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.s) {
            Text("You stay in control")
                .font(MobiusStyle.titleFont)
            Text("Manage your subscription from the möbius app or App Store.")
                .font(MobiusStyle.bodyFont)
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// Pricing stays with the action so the terms remain visible wherever the page is scrolled.
    ///
    /// Sign in with Apple is a branded control: Apple's guidelines allow black, white, or
    /// outlined only, so it cannot wear the app's accent. White is the variant meant for a
    /// dark background. The fade underneath is not a bar — it only keeps the last line of
    /// text from colliding with the capsule as the page scrolls past it.
    private var signupBoundary: some View {
        VStack(spacing: MobiusSpace.m) {
            billingDescription
                .font(MobiusStyle.bodyFont)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            if let cloudError = model.cloudError {
                Text(cloudError)
                    .font(MobiusStyle.captionFont)
                    .foregroundStyle(palette.danger)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
            if model.cloudAction.isRunning {
                HStack(spacing: MobiusSpace.s) {
                    ProgressView()
                    Text(model.cloudAction.label)
                }
                .font(MobiusStyle.controlFont)
                .frame(maxWidth: .infinity, minHeight: 50)
                .accessibilityElement(children: .combine)
            } else if let product {
                if model.hasCloudAccount {
                    Button("Subscribe for \(product.displayPrice) a month") {
                        Task {
                            if await model.purchaseCloud(product) { dismiss() }
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.extraLarge)
                    .frame(maxWidth: .infinity)
                } else {
                    SignInWithAppleButton(.continue) { request in
                        configureAppleRequest(request)
                    } onCompletion: { result in
                        completeAppleSignIn(result, product: product)
                    }
                    .signInWithAppleButtonStyle(.white)
                    .frame(maxWidth: .infinity, minHeight: 50)
                }
            } else if productLoadFailed {
                VStack(spacing: MobiusSpace.s) {
                    Text("The App Store price could not be loaded.")
                        .font(MobiusStyle.captionFont)
                        .foregroundStyle(palette.muted)
                    Button("Retry App Store") {
                        Task { await loadProduct() }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.large)
                }
                .frame(maxWidth: .infinity, minHeight: 50)
            } else {
                Button("Connecting to the App Store…") {}
                    .buttonStyle(.bordered)
                    .controlSize(.extraLarge)
                    .disabled(true)
                    .frame(maxWidth: .infinity)
            }
        }
        .frame(maxWidth: 680)
        .padding(.horizontal, MobiusSpace.l)
        .padding(.top, MobiusSpace.l)
        .padding(.bottom, MobiusSpace.s)
        .frame(maxWidth: .infinity)
        .background {
            LinearGradient(
                colors: [palette.canvas.opacity(0), palette.canvas],
                startPoint: .top,
                endPoint: .bottom
            )
            .ignoresSafeArea()
            .allowsHitTesting(false)
        }
    }

    private var billingDescription: Text {
        guard let product else {
            return Text(
                "Billed monthly. \(Text("Price shown at purchase.").foregroundStyle(palette.muted))"
            )
        }
        return Text(
            "\(product.displayPrice) a month. \(Text("Cancel anytime.").foregroundStyle(palette.muted))"
        )
    }

    private func loadProduct() async {
        guard product == nil else { return }
        productLoadFailed = false
        do {
            product = try await Product.products(for: [mobiusCloudMonthlyProductID]).first
            productLoadFailed = product == nil
        } catch {
            productLoadFailed = true
        }
    }

    private func configureAppleRequest(_ request: ASAuthorizationAppleIDRequest) {
        do {
            let nonce = try MobiusCloudAppleNonce.make()
            appleNonce = nonce
            request.requestedScopes = [.email]
            request.nonce = nonce.requestValue
        } catch {
            appleNonce = nil
            model.reportCloudSignInFailure()
        }
    }

    private func completeAppleSignIn(
        _ result: Result<ASAuthorization, Error>,
        product: Product
    ) {
        switch result {
        case .failure(let error):
            appleNonce = nil
            if let authorizationError = error as? ASAuthorizationError,
               authorizationError.code == .canceled {
                return
            }
            model.reportCloudSignInFailure()
        case .success(let authorization):
            guard let nonce = appleNonce,
                  let credential = authorization.credential as? ASAuthorizationAppleIDCredential,
                  let data = credential.authorizationCode,
                  let authorizationCode = String(data: data, encoding: .utf8)
            else {
                appleNonce = nil
                model.reportCloudSignInFailure()
                return
            }
            appleNonce = nil
            Task {
                if await model.signInAndPurchaseCloud(
                    authorizationCode: authorizationCode,
                    nonce: nonce.rawValue,
                    product: product
                ) {
                    dismiss()
                }
            }
        }
    }
}

private struct CloudBenefit: View {
    @Environment(\.mobiusPalette) private var palette
    let glyph: MobiusGlyph
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: MobiusSpace.m) {
            MobiusIcon(glyph, size: MobiusStyle.glyphLead, foreground: palette.accent)
            VStack(alignment: .leading, spacing: MobiusSpace.xs) {
                Text(title)
                    .font(MobiusStyle.controlFont)
                Text(detail)
                    .font(MobiusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.vertical, MobiusSpace.m)
    }
}
