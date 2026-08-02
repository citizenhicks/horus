import SwiftUI
import LucideIcons
#if os(iOS)
import UIKit
#else
import AppKit
#endif

enum HorusStyle {
    #if os(iOS)
    static let bodyFont: Font = .footnote
    static let controlFont: Font = .footnote.weight(.medium)
    #else
    static let bodyFont: Font = .system(size: 14)
    static let controlFont: Font = .system(size: 14, weight: .medium)
    #endif
    static let metadataFont: Font = .caption.monospaced()
    static let badgeFont: Font = .caption.weight(.medium)
    static let cardRadius: CGFloat = 22
    static let controlRadius: CGFloat = 9
    static let cardPadding: CGFloat = 14
    static let controlHeight: CGFloat = 30
    static let badgeHeight: CGFloat = 26
    static let iconSize: CGFloat = 16
    #if os(iOS)
    static let iconButtonSize: CGFloat = 44
    #else
    static let iconButtonSize: CGFloat = 32
    #endif
    static let borderWidth: CGFloat = 0.75
}

struct HorusIcon: View {
    let name: String
    var size = HorusStyle.iconSize
    var foreground: Color? = nil

    @ViewBuilder
    var body: some View {
        if let foreground {
            icon.foregroundStyle(foreground)
        } else {
            icon
        }
    }

    private var icon: some View {
        image
            .renderingMode(.template)
            .resizable()
            .scaledToFit()
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }

    private var image: Image {
        #if os(iOS)
        let image = UIImage(lucideId: name) ?? UIImage(lucideId: "circle-question-mark")!
        return Image(uiImage: image.withRenderingMode(.alwaysTemplate))
        #else
        let source = NSImage.image(lucideId: name) ?? NSImage.image(lucideId: "circle-question-mark")!
        let image = source.copy() as! NSImage
        image.isTemplate = true
        return Image(nsImage: image)
        #endif
    }
}

struct HorusLabel: View {
    let title: String
    let icon: String
    var iconColor: Color? = nil

    var body: some View {
        Label {
            Text(title)
        } icon: {
            HorusIcon(name: icon, foreground: iconColor)
        }
    }
}

extension Button where Label == HorusLabel {
    init(
        _ title: String,
        lucideIcon: String,
        role: ButtonRole? = nil,
        action: @escaping () -> Void
    ) {
        self.init(role: role, action: action) {
            HorusLabel(
                title: title,
                icon: lucideIcon,
                iconColor: role == .destructive ? .red : nil
            )
        }
    }
}

struct HorusPalette: Sendable {
    let canvas: Color
    let panel: Color
    let raised: Color
    let line: Color
    let accent: Color
    let accentSoft: Color
    let signal: Color
    let warning: Color
    let danger: Color
    let muted: Color

    init(_ scheme: ColorScheme) {
        if scheme == .dark {
            canvas = .black
            panel = Color(red: 0.090, green: 0.085, blue: 0.074)
            raised = Color(red: 0.130, green: 0.120, blue: 0.098)
            line = Color(red: 0.250, green: 0.235, blue: 0.190)
            accent = Color(red: 0.925, green: 0.660, blue: 0.270)
            accentSoft = Color(red: 0.250, green: 0.190, blue: 0.085)
            signal = Color(red: 0.425, green: 0.775, blue: 0.620)
            warning = Color(red: 0.930, green: 0.690, blue: 0.335)
            danger = Color(red: 0.900, green: 0.420, blue: 0.390)
            muted = Color(red: 0.600, green: 0.635, blue: 0.565)
        } else {
            canvas = Color(red: 0.985, green: 0.982, blue: 0.970)
            panel = Color(red: 0.945, green: 0.936, blue: 0.910)
            raised = Color(red: 0.975, green: 0.968, blue: 0.948)
            line = Color(red: 0.790, green: 0.750, blue: 0.660)
            accent = Color(red: 0.610, green: 0.335, blue: 0.055)
            accentSoft = Color(red: 0.925, green: 0.820, blue: 0.600)
            signal = Color(red: 0.100, green: 0.440, blue: 0.325)
            warning = Color(red: 0.650, green: 0.390, blue: 0.030)
            danger = Color(red: 0.650, green: 0.180, blue: 0.160)
            muted = Color(red: 0.390, green: 0.380, blue: 0.330)
        }
    }
}

extension EnvironmentValues {
    @Entry var horusPalette = HorusPalette(.dark)
}

struct HorusTheme: ViewModifier {
    @Environment(\.colorScheme) private var colorScheme

    func body(content: Content) -> some View {
        let palette = HorusPalette(colorScheme)
        content
            .environment(\.horusPalette, palette)
            .foregroundStyle(.primary)
            .tint(palette.accent)
            .font(HorusStyle.bodyFont)
    }
}

struct HorusBackdrop: View {
    @Environment(\.horusPalette) private var palette

    var body: some View {
        palette.canvas
            .ignoresSafeArea()
            .accessibilityHidden(true)
    }
}

struct HorusCard<Content: View>: View {
    let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(HorusStyle.cardPadding)
            .glassEffect(.regular, in: cardShape)
    }

    private var cardShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: HorusStyle.cardRadius, style: .continuous)
    }
}

struct HorusBadge: View {
    @Environment(\.horusPalette) private var palette
    let text: String
    var tone = "neutral"
    var symbol: String?
    var progress: Double?
    var interactive = false

    var body: some View {
        HStack(spacing: 6) {
            if let progress {
                ZStack {
                    Circle().stroke(palette.line.opacity(0.55), lineWidth: 2)
                    Circle()
                        .trim(from: 0, to: min(max(progress, 0), 1))
                        .stroke(palette.accent, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                        .rotationEffect(.degrees(-90))
                }
                .frame(width: 12, height: 12)
                .accessibilityHidden(true)
            }
            if let symbol { HorusIcon(name: symbol, size: 13, foreground: foreground) }
            Text(text).lineLimit(1)
        }
        .font(HorusStyle.badgeFont)
        .foregroundStyle(foreground)
        .padding(.horizontal, 11)
        .frame(height: HorusStyle.badgeHeight)
        .horusGlass(in: Capsule(), interactive: interactive)
    }

    private var foreground: Color {
        switch tone {
        case "success": palette.signal
        case "warning": palette.warning
        case "error": palette.danger
        default: palette.muted
        }
    }
}

struct HorusMenuLabel: View {
    let text: String
    var symbol: String?

    var body: some View {
        HStack(spacing: 6) {
            if let symbol { HorusIcon(name: symbol) }
            Text(text).lineLimit(1)
            HorusIcon(name: "chevrons-up-down", size: 13)
        }
        .font(HorusStyle.controlFont)
        .frame(height: HorusStyle.controlHeight)
        .contentShape(Rectangle())
    }
}

struct HorusIconButtonStyle: ButtonStyle {
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(HorusStyle.controlFont)
            .foregroundStyle(prominent ? Color.white : .primary)
            .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
            .horusGlass(
                in: Circle(),
                interactive: true,
                prominent: prominent
            )
            .opacity(configuration.isPressed ? 0.72 : 1)
    }
}

extension View {
    func horusGlass<S: Shape>(
        in shape: S,
        interactive: Bool = false,
        prominent: Bool = false
    ) -> some View {
        modifier(HorusGlassModifier(shape: shape, interactive: interactive, prominent: prominent))
    }

    func horusPopoverCard() -> some View {
        modifier(HorusPopoverCardModifier())
    }
}

private struct HorusGlassModifier<S: Shape>: ViewModifier {
    @Environment(\.horusPalette) private var palette
    let shape: S
    let interactive: Bool
    let prominent: Bool

    func body(content: Content) -> some View {
        let glass = prominent ? Glass.regular.tint(palette.accent) : Glass.regular
        if interactive {
            content.glassEffect(glass.interactive(), in: shape)
        } else {
            content.glassEffect(glass, in: shape)
        }
    }
}

private struct HorusPopoverCardModifier: ViewModifier {
    func body(content: Content) -> some View {
        content
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(HorusStyle.cardPadding)
            .glassEffect(.regular, in: cardShape)
    }

    private var cardShape: RoundedRectangle {
        RoundedRectangle(cornerRadius: HorusStyle.cardRadius, style: .continuous)
    }
}

struct SectionHeading: View {
    @Environment(\.horusPalette) private var palette
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title)
                .font(.headline.weight(.semibold))
            Text(detail)
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
        }
    }
}

extension View {
    func horusTheme() -> some View { modifier(HorusTheme()) }
}
