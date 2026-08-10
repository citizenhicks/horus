import StoreKit
import SwiftUI

private let horusCloudMonthlyProductID = "app.horus.client.cloud.monthly"

struct HorusCloudOfferButton: View {
    @Environment(\.horusPalette) private var palette
    @State private var product: Product?
    @State private var showsOffer = false

    var body: some View {
        Button {
            showsOffer = true
        } label: {
            HStack(spacing: 10) {
                HorusIcon(.globe02, foreground: palette.accent)
                title
                    .font(HorusStyle.controlFont)
                    .multilineTextAlignment(.leading)
                Spacer(minLength: 4)
                HorusIcon(.caretRight, size: 12, foreground: palette.muted)
            }
            .frame(maxWidth: .infinity, minHeight: HorusStyle.iconButtonSize)
            .contentShape(Rectangle())
        }
        .buttonStyle(.horusGlass)
        .buttonBorderShape(.capsule)
        .controlSize(.large)
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
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 10) {
                HorusIcon(.globe02, size: 22, foreground: palette.accent)
                    .frame(width: 44, height: 44)
                    .background(palette.accentSoft.opacity(0.65), in: Circle())
                HorusBadge(text: "7 days free", tone: "success", glyph: .sealCheck)
            }
            Text("Your private gateway, managed by Horus.")
                .font(.largeTitle.weight(.bold))
                .fixedSize(horizontal: false, vertical: true)
            Text(
                "Skip server setup without giving up control. We provision, secure, and maintain a gateway scoped to your account."
            )
                .font(.title3)
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

    /// The price sits directly above the button that charges it, in the app's own accent
    /// rather than a hand-rolled black rectangle: an imitation of Apple's button reads as a
    /// worse version of it, and the real one cannot front a signup that does nothing yet.
    private var signupBoundary: some View {
        VStack(spacing: 10) {
            billingDescription
                .font(HorusStyle.controlFont)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                showsUnavailable = true
            } label: {
                Label("Continue with Apple", systemImage: "apple.logo")
                    .font(.headline)
            }
            .horusProminentButton()
            .buttonBorderShape(.capsule)
            .controlSize(.large)
            .buttonSizing(.flexible)
            Text("Signup and billing are not enabled in this beta.")
                .font(HorusStyle.metadataFont)
                .foregroundStyle(palette.muted)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: 680)
        .padding(.horizontal, 20)
        .padding(.top, 14)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity)
        .background {
            // The bar carries the page's own surface, and a hairline says where the
            // scrolling content ends rather than a material change nobody else uses.
            palette.canvas
                .overlay(alignment: .top) {
                    Rectangle().fill(palette.line).frame(height: HorusStyle.borderWidth)
                }
                .ignoresSafeArea()
        }
    }

    private var billingDescription: Text {
        guard let product else {
            return Text("7 days free, then billed monthly. ")
                + Text("Your App Store price is shown before you pay.")
                .foregroundColor(palette.muted)
        }
        return Text("7 days free, then \(product.displayPrice) a month. ")
            + Text("Cancel anytime.").foregroundColor(palette.muted)
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
