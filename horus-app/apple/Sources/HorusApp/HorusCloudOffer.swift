import StoreKit
import SwiftUI

private let horusCloudMonthlyProductID = "app.horus.client.cloud.monthly"

struct HorusCloudOfferButton: View {
    @Environment(\.horusPalette) private var palette
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
                Image("HorusLogo")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 20, height: 20)
                    .accessibilityHidden(true)
            }
            .font(HorusStyle.controlFont)
        }
        .buttonStyle(.horusGlass)
        .tint(palette.accent)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
        .buttonSizing(.flexible)
        .task { await loadProduct() }
        .sheet(isPresented: $showsOffer) {
            HorusCloudOfferSheet(product: product)
                .presentationDragIndicator(.visible)
        }
        .accessibilityHint("Explains the managed Horus Cloud subscription")
    }

    private var title: Text {
        guard let product else { return Text("Connect to Horus Cloud") }
        return Text("Connect to Horus Cloud for \(product.displayPrice) per month")
    }

    @MainActor
    private func loadProduct() async {
        guard product == nil else { return }
        product = try? await Product.products(for: [horusCloudMonthlyProductID]).first
    }
}

private struct HorusCloudOfferSheet: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.horusPalette) private var palette
    @State private var showsUnavailable = false
    let product: Product?

    var body: some View {
        NavigationStack {
            ZStack {
                HorusBackdrop()
                ScrollView {
                    VStack(alignment: .leading, spacing: 28) {
                        hero
                        offerDetails
                        controlNote
                    }
                    .frame(maxWidth: 680, alignment: .leading)
                    .padding(.horizontal, 20)
                    .padding(.top, 18)
                    .padding(.bottom, 28)
                    .frame(maxWidth: .infinity)
                }
                .scrollIndicators(.hidden)
            }
            .navigationTitle("Horus Cloud")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Close") { dismiss() }
                }
            }
            .safeAreaInset(edge: .bottom) { signupBoundary }
        }
        .alert("Cloud signup is not available yet", isPresented: $showsUnavailable) {
            Button("OK", role: .cancel) {}
        } message: {
            Text(
                "This beta includes the Horus Cloud offer and storefront pricing preview. Sign in with Apple and subscription purchase are not connected yet."
            )
        }
    }

    /// One voice per line: a mark, the promise, and the shape of the offer. The price is not
    /// here — it belongs beside the button that charges it, not at the top in accent.
    private var hero: some View {
        VStack(alignment: .leading, spacing: 18) {
            // The app's own mark, not a stock globe. The free trial is not announced twice:
            // it is stated once, next to the price it applies to.
            HorusComposingOrb()
                .frame(width: 64, height: 64)
                .frame(maxWidth: .infinity)
                .accessibilityHidden(true)
            Text("Your private gateway, managed by Horus.")
                .font(.largeTitle.weight(.bold))
                .fixedSize(horizontal: false, vertical: true)
            Text(
                "Skip server setup without giving up control. We provision, secure, and maintain a gateway scoped to your account."
            )
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var offerDetails: some View {
        HorusCard {
            VStack(alignment: .leading, spacing: 0) {
                CloudBenefit(
                    glyph: .sparkle,
                    title: "2 million Luna tokens included",
                    detail: "Use up to 2 million tokens each month with Luna, the default Horus model."
                )
                Divider().padding(.leading, 42)
                CloudBenefit(
                    glyph: .setup01,
                    title: "Fast, modular harness",
                    detail: "Choose the providers, tools, and capabilities you want while Horus keeps the runtime lean."
                )
                Divider().padding(.leading, 42)
                CloudBenefit(
                    glyph: .key,
                    title: "Bring your own keys",
                    detail: "Connect your own provider credentials whenever you need another model or account."
                )
                Divider().padding(.leading, 42)
                CloudBenefit(
                    glyph: .shieldCheck,
                    title: "Encrypted and user-scoped",
                    detail: "Your gateway, credentials, and cloud data stay isolated to your account."
                )
            }
        }
    }

    private var controlNote: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text("You stay in control")
                .font(.headline)
            Text("Manage the gateway or permanently delete your cloud from the Horus app or website.")
                .font(HorusStyle.bodyFont)
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
        VStack(spacing: 10) {
            billingDescription
                .font(HorusStyle.bodyFont)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Text("Signup and billing are not enabled in this beta.")
                .font(.footnote)
                .foregroundStyle(palette.muted)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                showsUnavailable = true
            } label: {
                Label("Continue with Apple", systemImage: "apple.logo")
                    .font(.headline)
                    .foregroundStyle(.black)
                    .frame(maxWidth: .infinity, minHeight: 50)
                    .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .background(.white, in: Capsule())
            .shadow(color: .black.opacity(0.32), radius: 14, y: 6)
        }
        .frame(maxWidth: 680)
        .padding(.horizontal, 20)
        .padding(.top, 20)
        .padding(.bottom, 6)
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
                "7 days free, then billed monthly. \(Text("Price shown at purchase.").foregroundStyle(palette.muted))"
            )
        }
        return Text(
            "7 days free, then \(product.displayPrice) a month. \(Text("Cancel anytime.").foregroundStyle(palette.muted))"
        )
    }
}

private struct CloudBenefit: View {
    @Environment(\.horusPalette) private var palette
    let glyph: HorusGlyph
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            HorusIcon(glyph, size: 20, foreground: palette.accent)
                .frame(width: 30, height: 30)
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(HorusStyle.controlFont)
                Text(detail)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(palette.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.vertical, 13)
    }
}
