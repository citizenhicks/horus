import SwiftUI

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

/// One HugeIcons glyph, vendored into the asset catalog under `hi.<name>`.
///
/// A type rather than a raw asset name: a missing SF Symbol at least logs, but a misspelled
/// asset name draws nothing at all and says nothing about it, so the names are worth holding
/// the compiler to. Add a case only alongside the matching imageset.
struct HorusGlyph: Hashable {
    let asset: String

    private init(_ asset: String) { self.asset = asset }

    static let arrowCircleUp = Self("hi.arrowCircleUp")
    static let arrowClockwise = Self("hi.arrowClockwise")
    static let arrowDown = Self("hi.arrowDown")
    static let arrowUp = Self("hi.arrowUp")
    static let brain = Self("hi.brain")
    static let calendarDots = Self("hi.calendarDots")
    static let caretDown = Self("hi.caretDown")
    static let caretRight = Self("hi.caretRight")
    static let caretUp = Self("hi.caretUp")
    static let caretUpDown = Self("hi.caretUpDown")
    static let cellTower = Self("hi.cellTower")
    static let chatCircle = Self("hi.chatCircle")
    static let chatDots = Self("hi.chatDots")
    static let chatGpt = Self("hi.chatGpt")
    static let chatsCircle = Self("hi.chatsCircle")
    static let check = Self("hi.check")
    static let checkCircle = Self("hi.checkCircle")
    static let claude = Self("hi.claude")
    static let clock = Self("hi.clock")
    static let copy = Self("hi.copy")
    static let cpu = Self("hi.cpu")
    static let deepseek = Self("hi.deepseek")
    static let dotsThree = Self("hi.dotsThree")
    static let fileMagnifyingGlass = Self("hi.fileMagnifyingGlass")
    static let fileText = Self("hi.fileText")
    static let fingerprint = Self("hi.fingerprint")
    static let floppyDisk = Self("hi.floppyDisk")
    static let folder = Self("hi.folder")
    static let folderPlus = Self("hi.folderPlus")
    static let gear = Self("hi.gear")
    static let gitBranch = Self("hi.gitBranch")
    static let handPalm = Self("hi.handPalm")
    static let hardDrives = Self("hi.hardDrives")
    static let info = Self("hi.info")
    static let key = Self("hi.key")
    static let link = Self("hi.link")
    static let lockOpen = Self("hi.lockOpen")
    static let magnifyingGlass = Self("hi.magnifyingGlass")
    static let menu = Self("hi.menu")
    static let moon = Self("hi.moon")
    static let notePencil = Self("hi.notePencil")
    static let path = Self("hi.path")
    static let pencilSimple = Self("hi.pencilSimple")
    static let playFill = Self("hi.playFill")
    static let plugsConnected = Self("hi.plugsConnected")
    static let plus = Self("hi.plus")
    static let pushPin = Self("hi.pushPin")
    static let pushPinSlash = Self("hi.pushPinSlash")
    static let question = Self("hi.question")
    static let robot = Self("hi.robot")
    static let sealCheck = Self("hi.sealCheck")
    static let shieldCheck = Self("hi.shieldCheck")
    static let sidebarSimple = Self("hi.sidebarSimple")
    static let signIn = Self("hi.signIn")
    static let slidersHorizontal = Self("hi.slidersHorizontal")
    static let sparkle = Self("hi.sparkle")
    static let squaresFour = Self("hi.squaresFour")
    static let stopFill = Self("hi.stopFill")
    static let terminalWindow = Self("hi.terminalWindow")
    static let trash = Self("hi.trash")
    static let userFocus = Self("hi.userFocus")
    static let warning = Self("hi.warning")
    static let warningOctagon = Self("hi.warningOctagon")
    static let x = Self("hi.x")
    static let xCircle = Self("hi.xCircle")
}

struct HorusIcon: View {
    let glyph: HorusGlyph
    var size = HorusStyle.iconSize
    var foreground: Color? = nil

    init(_ glyph: HorusGlyph, size: CGFloat = HorusStyle.iconSize, foreground: Color? = nil) {
        self.glyph = glyph
        self.size = size
        self.foreground = foreground
    }

    @ViewBuilder
    var body: some View {
        // The asset carries `template-rendering-intent`, so this tints from the foreground
        // style the way a symbol does. Unlike a symbol it has no intrinsic text size, which is
        // why every glyph is drawn into an explicit square instead of following the font.
        let icon = Image(glyph.asset)
            .renderingMode(.template)
            .resizable()
            .aspectRatio(contentMode: .fit)
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
    let glyph: HorusGlyph
    var iconColor: Color? = nil
    var iconSize = HorusStyle.iconSize

    var body: some View {
        Label {
            Text(title)
        } icon: {
            HorusIcon(glyph, size: iconSize, foreground: iconColor)
        }
    }
}

/// Uses SF Symbols for native macOS menus, whose tinting is owned by AppKit.
struct HorusPlatformMenuLabel: View {
    let title: String
    let glyph: HorusGlyph
    let systemImage: String

    var body: some View {
        #if os(macOS)
        Label(title, systemImage: systemImage)
        #else
        HorusLabel(title: title, glyph: glyph)
        #endif
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
        .buttonStyle(.horusGlass)
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
    let glyph: HorusGlyph
    var detail: String?

    var body: some View {
        ContentUnavailableView {
            HorusLabel(title: title, glyph: glyph, iconSize: 32)
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
        glyph: HorusGlyph,
        role: ButtonRole? = nil,
        action: @escaping () -> Void
    ) {
        self.init(role: role, action: action) {
            HorusLabel(title: title, glyph: glyph)
        }
    }
}

/// Draws the gateway's `FrontendSymbol` vocabulary in HugeIcons.
///
/// The protocol names what a glyph stands for and leaves the artwork to each frontend, so
/// this table is the Apple client's half of that contract: one entry per `FrontendSymbol`
/// variant, and the gateway never names an icon itself. Keep it in step with the enum in
/// `src/protocol/mod.rs` — a variant with no entry here falls back to `placeholder`.
///
/// `FrontendSymbol::Custom` has no entry by definition. It carries a name from outside the
/// protocol's vocabulary, which this app has no artwork for, so it draws the placeholder.
enum HorusSymbol {
    private struct Artwork {
        let glyph: HorusGlyph
        let systemImage: String
    }

    private static let placeholder = Artwork(
        glyph: .question,
        systemImage: "questionmark.square.dashed"
    )

    static func glyph(for symbol: String) -> HorusGlyph {
        artwork(for: symbol).glyph
    }

    static func systemImage(for symbol: String) -> String {
        artwork(for: symbol).systemImage
    }

    /// One entry per `FrontendSymbol` variant.
    private static let vocabulary: [String: Artwork] = [
        "agent": Artwork(glyph: .robot, systemImage: "person.fill"),
        "brain": Artwork(glyph: .brain, systemImage: "brain.head.profile"),
        "branch": Artwork(glyph: .gitBranch, systemImage: "arrow.trianglehead.branch"),
        "chat": Artwork(glyph: .chatCircle, systemImage: "text.bubble"),
        "chat_gpt": Artwork(glyph: .chatGpt, systemImage: "bubble.left.and.text.bubble.right"),
        "claude": Artwork(glyph: .claude, systemImage: "sparkles"),
        "deepseek": Artwork(glyph: .deepseek, systemImage: "waveform.path.ecg"),
        "delete": Artwork(glyph: .trash, systemImage: "trash"),
        "edit": Artwork(glyph: .pencilSimple, systemImage: "pencil"),
        "moon": Artwork(glyph: .moon, systemImage: "moon"),
        "promote": Artwork(glyph: .arrowCircleUp, systemImage: "arrow.up.circle"),
        "route": Artwork(glyph: .path, systemImage: "point.3.connected.trianglepath.dotted"),
        "search": Artwork(glyph: .magnifyingGlass, systemImage: "magnifyingglass"),
        "sparkle": Artwork(glyph: .sparkle, systemImage: "sparkles"),
        "storage": Artwork(glyph: .hardDrives, systemImage: "externaldrive.connected.to.line.below"),
    ]

    private static func artwork(for symbol: String) -> Artwork {
        vocabulary[symbol] ?? placeholder
    }
}

struct HorusPalette: Sendable {
    let canvas: Color
    /// One step under `canvas`, for the surface the canvas slides over: the compact drawer
    /// puts the sidebar directly behind the page, and two surfaces at the same value read as
    /// one sheet however clean the cut between them is.
    let recessed: Color
    let panel: Color
    let raised: Color
    let line: Color
    /// Strokes, rings, and marks drawn *in* the accent. Not a background for text.
    let accent: Color
    /// Fill behind `onAccent` labels, darker than `accent` so the pair clears WCAG AA.
    ///
    /// Glass composites its tint with whatever sits behind it, so a fill that only just
    /// clears on paper drifts under one: the light scheme lightens the result and loses
    /// contrast, the dark scheme darkens it and gains. Light therefore carries the extra
    /// headroom, the same way `signal`, `warning`, and `danger` are already darkened there.
    let accentFill: Color
    let accentSoft: Color
    let signal: Color
    let warning: Color
    let danger: Color
    let muted: Color
    /// Label colour for anything filled with `accentFill`.
    let onAccent: Color

    // Nord (nordtheme.com): the canvas sits below Polar Night so nord0–nord3 read as
    // raised surfaces, with the darker Frost blue as the accent.
    init(_ scheme: ColorScheme) {
        onAccent = .nord6
        if scheme == .dark {
            canvas = Color(red: 0.141, green: 0.161, blue: 0.200)
            recessed = Color(red: 0.094, green: 0.106, blue: 0.133)
            panel = .nord0
            raised = .nord1
            line = .nord3
            accent = .nord10
            // 4.84:1 against onAccent, and the dark backdrop only deepens it under glass.
            accentFill = Color(red: 0.298, green: 0.416, blue: 0.557)
            accentSoft = Color(red: 0.227, green: 0.278, blue: 0.349)
            signal = .nord14
            warning = .nord13
            danger = .nord11
            muted = Color(red: 0.541, green: 0.588, blue: 0.671)
        } else {
            canvas = .nord6
            recessed = .nord5
            panel = .nord5
            raised = Color(red: 0.965, green: 0.973, blue: 0.984)
            line = .nord4
            accent = .nord10
            // 6.15:1 against onAccent: the light backdrop lightens the tint under glass,
            // so the extra headroom is what keeps the composited result above 4.5:1.
            accentFill = Color(red: 0.239, green: 0.353, blue: 0.494)
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
            .buttonStyle(.horusAutomatic)
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
    var glyph: HorusGlyph?
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
            if let glyph {
                HorusIcon(glyph, size: 13, foreground: foreground)
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
    var glyph: HorusGlyph?

    var body: some View {
        HStack(spacing: 6) {
            if let glyph { HorusIcon(glyph) }
            Text(text).lineLimit(1)
            HorusIcon(.caretUpDown, size: 13)
        }
        .font(HorusStyle.controlFont)
        .frame(height: HorusStyle.controlHeight)
        .contentShape(Rectangle())
    }
}

struct HorusFeedbackButtonStyle<Base: PrimitiveButtonStyle>: PrimitiveButtonStyle {
    let base: Base

    @ViewBuilder
    func makeBody(configuration: Configuration) -> some View {
        #if os(iOS)
        FeedbackButton(configuration: configuration, base: base)
        #else
        base.makeBody(configuration: configuration)
        #endif
    }

    #if os(iOS)
    private struct FeedbackButton: View {
        @State private var feedback = false
        let configuration: PrimitiveButtonStyleConfiguration
        let base: Base

        var body: some View {
            Button(role: configuration.role) {
                feedback.toggle()
                configuration.trigger()
            } label: {
                configuration.label
            }
            .buttonStyle(base)
            .sensoryFeedback(.impact(weight: .light), trigger: feedback)
        }
    }
    #endif
}

extension PrimitiveButtonStyle where Self == HorusFeedbackButtonStyle<DefaultButtonStyle> {
    static var horusAutomatic: Self { Self(base: DefaultButtonStyle()) }
}

extension PrimitiveButtonStyle where Self == HorusFeedbackButtonStyle<PlainButtonStyle> {
    static var horusPlain: Self { Self(base: PlainButtonStyle()) }
}

extension PrimitiveButtonStyle where Self == HorusFeedbackButtonStyle<GlassButtonStyle> {
    static var horusGlass: Self { Self(base: GlassButtonStyle()) }
}

extension PrimitiveButtonStyle where Self == HorusFeedbackButtonStyle<GlassProminentButtonStyle> {
    static var horusGlassProminent: Self { Self(base: GlassProminentButtonStyle()) }
}

struct HorusIconButtonStyle: ButtonStyle {
    var prominent = false

    func makeBody(configuration: Configuration) -> some View {
        IconButton(label: configuration.label, isPressed: configuration.isPressed, prominent: prominent)
    }

    private struct IconButton: View {
        @Environment(\.horusPalette) private var palette
        // A custom style gets no automatic disabled treatment: without this the send button
        // keeps a full-strength accent glyph on a circle that no longer responds.
        @Environment(\.isEnabled) private var isEnabled
        let label: ButtonStyleConfiguration.Label
        let isPressed: Bool
        let prominent: Bool

        var body: some View {
            label
                .font(HorusStyle.controlFont)
                .foregroundStyle(foreground)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                // A glass effect adds no hit area, so without this the tap target is the
                // 16pt glyph, not the 44pt circle: unusable by touch, fine with a cursor.
                .contentShape(Circle())
                .horusGlass(in: Circle(), interactive: isEnabled, prominent: prominent && isEnabled)
                .opacity(isPressed ? 0.72 : 1)
                #if os(iOS)
                .sensoryFeedback(.impact(weight: .light), trigger: isPressed) { _, pressed in pressed }
                #endif
        }

        private var foreground: Color {
            guard isEnabled else { return palette.muted }
            return prominent ? palette.onAccent : .primary
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
        // `.glassProminent` fills from the tint, so it needs the accessible one rather than
        // the global tint `HorusTheme` sets for switches, pickers, and links.
        content
            .buttonStyle(.horusGlassProminent)
            .tint(palette.accentFill)
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
        let glass = prominent ? Glass.regular.tint(palette.accentFill) : Glass.regular
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
