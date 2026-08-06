import SwiftUI
#if os(macOS)
import AppKit
#else
import UIKit
#endif

enum HorusStyle {
    #if os(iOS)
    static let bodyFont: Font = .body
    static let controlFont: Font = .body.weight(.medium)
    static let metadataFont: Font = .footnote.monospaced()
    static let badgeFont: Font = .footnote.weight(.medium)
    #else
    static let bodyFont: Font = .system(size: 14)
    static let controlFont: Font = .system(size: 14, weight: .medium)
    static let metadataFont: Font = .caption.monospaced()
    static let badgeFont: Font = .caption.weight(.medium)
    #endif
    static let cardRadius: CGFloat = 22
    static let controlRadius: CGFloat = 9
    static let cardShape = RoundedRectangle(cornerRadius: cardRadius, style: .continuous)
    static let controlShape = RoundedRectangle(cornerRadius: controlRadius, style: .continuous)
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
    let systemName: String
    var size = HorusStyle.iconSize
    var foreground: Color? = nil

    @ViewBuilder
    var body: some View {
        let icon = Image(systemName: systemName)
            .symbolRenderingMode(.monochrome)
            .font(.system(size: size, weight: .regular))
            .frame(width: size, height: size)
            .accessibilityHidden(true)
        if let foreground {
            icon.foregroundStyle(foreground)
        } else {
            icon
        }
    }
}

struct HorusLabel: View {
    let title: String
    let systemImage: String
    var iconColor: Color? = nil
    var iconSize = HorusStyle.iconSize

    var body: some View {
        Label {
            Text(title)
        } icon: {
            HorusIcon(systemName: systemImage, size: iconSize, foreground: iconColor)
        }
    }
}

/// Row of capsule actions. Rows of several actions drop their labels on a narrow
/// screen; a single action keeps its label and goes full width instead.
struct HorusActionRow<Content: View>: View {
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif
    var collapsesToIcons = false
    @ViewBuilder let content: Content

    var body: some View {
        Group {
            if iconsOnly {
                HStack(spacing: 8) { content }
                    .labelStyle(.iconOnly)
                    .buttonBorderShape(.circle)
            } else {
                #if os(iOS)
                // Full width keeps the label: measuring for a horizontal fit either
                // hyphenates or truncates it away.
                VStack(spacing: 8) { content }.buttonSizing(.flexible)
                #else
                HStack(spacing: 8) { content }
                #endif
            }
        }
        .frame(maxWidth: .infinity)
        .lineLimit(1)
        .buttonStyle(.glass)
        .buttonBorderShape(iconsOnly ? .circle : .capsule)
        .controlSize(.large)
    }

    private var iconsOnly: Bool {
        #if os(iOS)
        collapsesToIcons && horizontalSizeClass == .compact
        #else
        false
        #endif
    }
}

struct HorusUnavailable: View {
    let title: String
    let systemImage: String
    var detail: String?

    var body: some View {
        ContentUnavailableView {
            HorusLabel(title: title, systemImage: systemImage, iconSize: 32)
        } description: {
            if let detail { Text(detail) }
        }
        // Reads as a page, not as a list row, when it stands in for a form's content.
        .listRowBackground(Color.clear)
        .listRowSeparator(.hidden)
    }
}

extension Button where Label == HorusLabel {
    init(
        _ title: String,
        systemImage: String,
        role: ButtonRole? = nil,
        action: @escaping () -> Void
    ) {
        self.init(role: role, action: action) {
            HorusLabel(title: title, systemImage: systemImage)
        }
    }
}

/// Resolves the gateway's symbol names to SF Symbols.
///
/// The vocabulary is open — middleware and frontend plugins send whatever they like — and
/// three conventions have accumulated in it: Phosphor names (`magnifying-glass`), semantic
/// aliases (`delete`), and names that are already SF Symbols (`pencil`, sent by the CLI
/// dashboard). A switch listing every name meant the third kind fell through to the
/// placeholder, so names are now tried against SF Symbols directly and the table below
/// carries only the ones spelled differently.
enum HorusSymbol {
    static let placeholder = "questionmark.square.dashed"

    static func systemName(for semanticName: String) -> String {
        if let alias = aliases[semanticName] { return alias }
        if isSystemSymbol(semanticName) { return semanticName }
        // Phosphor hyphenates what SF Symbols runs together: magnifying-glass, arrows-clockwise.
        let collapsed = semanticName.replacingOccurrences(of: "-", with: "")
        if collapsed != semanticName, isSystemSymbol(collapsed) { return collapsed }
        return placeholder
    }

    /// Only the names SF Symbols spells differently; the rest resolve on their own.
    private static let aliases: [String: String] = [
        "brain": "brain.head.profile",
        "chat-circle": "text.bubble",
        "delete": "trash",
        "edit": "pencil",
        "fork": "arrow.trianglehead.branch",
        "hard-drives": "externaldrive.connected.to.line.below",
        "path": "point.3.connected.trianglepath.dotted",
        "promote": "arrow.up.circle",
        "robot": "person.fill",
        "sparkle": "sparkles",
    ]

    private static func isSystemSymbol(_ name: String) -> Bool {
        #if os(macOS)
        NSImage(systemSymbolName: name, accessibilityDescription: nil) != nil
        #else
        UIImage(systemName: name) != nil
        #endif
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
    /// Label colour for anything filled with `accent`.
    let onAccent: Color

    // Nord (nordtheme.com): the canvas sits below Polar Night so nord0–nord3 read as
    // raised surfaces, with the darker Frost blue as the accent.
    init(_ scheme: ColorScheme) {
        onAccent = .nord6
        if scheme == .dark {
            canvas = Color(red: 0.141, green: 0.161, blue: 0.200)
            panel = .nord0
            raised = .nord1
            line = .nord3
            accent = .nord10
            accentSoft = Color(red: 0.227, green: 0.278, blue: 0.349)
            signal = .nord14
            warning = .nord13
            danger = .nord11
            muted = Color(red: 0.541, green: 0.588, blue: 0.671)
        } else {
            canvas = .nord6
            panel = .nord5
            raised = Color(red: 0.965, green: 0.973, blue: 0.984)
            line = .nord4
            accent = .nord10
            accentSoft = Color(red: 0.831, green: 0.871, blue: 0.918)
            signal = Color(red: 0.353, green: 0.482, blue: 0.243)
            warning = Color(red: 0.565, green: 0.435, blue: 0.153)
            danger = Color(red: 0.639, green: 0.263, blue: 0.310)
            muted = .nord3
        }
    }

    func tone(_ tone: String) -> Color {
        switch tone {
        case "success": signal
        case "warning": warning
        case "error": danger
        default: muted
        }
    }
}

private extension Color {
    static let nord0 = Color(red: 0.180, green: 0.204, blue: 0.251)
    static let nord1 = Color(red: 0.231, green: 0.259, blue: 0.322)
    static let nord2 = Color(red: 0.263, green: 0.298, blue: 0.369)
    static let nord3 = Color(red: 0.298, green: 0.337, blue: 0.416)
    static let nord4 = Color(red: 0.847, green: 0.871, blue: 0.914)
    static let nord5 = Color(red: 0.898, green: 0.914, blue: 0.941)
    static let nord6 = Color(red: 0.925, green: 0.937, blue: 0.957)
    static let nord8 = Color(red: 0.533, green: 0.753, blue: 0.816)
    static let nord10 = Color(red: 0.369, green: 0.506, blue: 0.675)
    static let nord11 = Color(red: 0.749, green: 0.380, blue: 0.416)
    static let nord13 = Color(red: 0.922, green: 0.796, blue: 0.545)
    static let nord14 = Color(red: 0.639, green: 0.745, blue: 0.549)
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
            .glassEffect(.regular, in: HorusStyle.cardShape)
    }
}

struct HorusBadge: View {
    @Environment(\.horusPalette) private var palette
    let text: String
    var tone = "neutral"
    var systemImage: String?
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
            if let systemImage {
                HorusIcon(systemName: systemImage, size: 13, foreground: foreground)
            }
            if !text.isEmpty { Text(text).lineLimit(1) }
        }
        .font(HorusStyle.badgeFont)
        .foregroundStyle(foreground)
        .padding(.horizontal, 11)
        .frame(height: HorusStyle.badgeHeight)
        .horusGlass(in: Capsule(), interactive: interactive)
    }

    private var foreground: Color { palette.tone(tone) }
}

struct HorusMenuLabel: View {
    let text: String
    var systemImage: String?

    var body: some View {
        HStack(spacing: 6) {
            if let systemImage { HorusIcon(systemName: systemImage) }
            Text(text).lineLimit(1)
            HorusIcon(systemName: "chevron.up.chevron.down", size: 13)
        }
        .font(HorusStyle.controlFont)
        .frame(height: HorusStyle.controlHeight)
        .contentShape(Rectangle())
    }
}

struct HorusIconButtonStyle: ButtonStyle {
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        IconButton(label: configuration.label, isPressed: configuration.isPressed, prominent: prominent)
    }

    private struct IconButton: View {
        @Environment(\.horusPalette) private var palette
        let label: ButtonStyleConfiguration.Label
        let isPressed: Bool
        let prominent: Bool

        var body: some View {
            label
                .font(HorusStyle.controlFont)
                .foregroundStyle(prominent ? palette.onAccent : .primary)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                // A glass effect adds no hit area, so without this the tap target is the
                // 16pt glyph, not the 44pt circle: unusable by touch, fine with a cursor.
                .contentShape(Circle())
                .horusGlass(in: Circle(), interactive: true, prominent: prominent)
                .opacity(isPressed ? 0.72 : 1)
                #if os(iOS)
                .sensoryFeedback(.impact(weight: .light), trigger: isPressed) { _, pressed in pressed }
                #endif
        }
    }
}

/// A prominent button with a label that stays legible on the accent in both schemes.
///
/// iOS draws `.glassProminent` from the tint this app sets, so the label colour and the
/// fill agree. macOS lets the system own that fill: it desaturates for an inactive window
/// and can follow the accent colour chosen in System Settings, neither of which the label
/// colour here knows about, so a label picked for the Nord accent ends up on a surface the
/// app never chose. Painting the fill in the style keeps the pair together — the same
/// `Glass.regular.tint(...)` path `HorusIconButtonStyle` already uses on both platforms.
private struct HorusProminentButton: ViewModifier {
    @Environment(\.horusPalette) private var palette

    func body(content: Content) -> some View {
        #if os(macOS)
        content.buttonStyle(HorusProminentButtonStyle())
        #else
        content
            .buttonStyle(.glassProminent)
            .foregroundStyle(palette.onAccent)
        #endif
    }
}

#if os(macOS)
struct HorusProminentButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        ProminentLabel(label: configuration.label, isPressed: configuration.isPressed)
    }

    private struct ProminentLabel: View {
        @Environment(\.horusPalette) private var palette
        // A custom style gets no automatic disabled treatment: without this the label keeps
        // its full-strength accent colour on a button that no longer responds.
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.controlSize) private var controlSize
        let label: ButtonStyleConfiguration.Label
        let isPressed: Bool

        var body: some View {
            label
                .font(HorusStyle.controlFont)
                .foregroundStyle(isEnabled ? palette.onAccent : palette.muted)
                .padding(.horizontal, 14)
                .frame(height: height)
                // A glass effect adds no hit area of its own.
                .contentShape(Capsule())
                .horusGlass(in: Capsule(), interactive: isEnabled, prominent: isEnabled)
                .opacity(isPressed ? 0.72 : 1)
        }

        // `buttonBorderShape` and `controlSize` only reach the built-in styles, so the
        // call sites asking for a large capsule are honoured here instead.
        private var height: CGFloat {
            switch controlSize {
            case .large, .extraLarge: 36
            default: HorusStyle.controlHeight
            }
        }
    }
}
#endif

extension View {
    func horusProminentButton() -> some View { modifier(HorusProminentButton()) }

    /// Lets a row of badges scroll instead of squeezing when it outgrows the width.
    func scrollableRow() -> some View {
        ScrollView(.horizontal) {
            fixedSize(horizontal: true, vertical: false)
        }
        // ponytail: centering can snap as badges arrive; keep one render until native overflow
        // alignment can preserve state without duplicating the controls.
        .defaultScrollAnchor(.center, for: .alignment)
        .scrollIndicators(.hidden)
        .scrollBounceBehavior(.basedOnSize)
    }

    func horusGlass<S: Shape>(
        in shape: S,
        interactive: Bool = false,
        prominent: Bool = false
    ) -> some View {
        modifier(HorusGlassModifier(shape: shape, interactive: interactive, prominent: prominent))
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

struct SectionHeading: View {
    @Environment(\.horusPalette) private var palette
    let title: String
    let detail: String

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title)
                .font(.headline)
            Text(detail)
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
        }
    }
}

extension View {
    func horusTheme() -> some View { modifier(HorusTheme()) }
}
