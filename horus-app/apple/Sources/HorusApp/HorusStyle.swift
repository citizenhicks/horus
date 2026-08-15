import SwiftUI

/// Every gap in the app — stack spacing and padding alike — is one of these six steps.
/// A gap that is not on the scale is drift: it reads as an accident beside the rows above
/// and below it. `0` stays 0 where a stack is deliberately flush.
enum HorusSpace {
    /// Two lines that belong to one another: a name over its path.
    static let xxs: CGFloat = 2
    /// Inside one label or badge.
    static let xs: CGFloat = 4
    /// The default: a glyph and its text, one row and the next.
    static let s: CGFloat = 8
    /// Between blocks inside a card.
    static let m: CGFloat = 12
    /// Screen margins, and the gap between cards.
    static let l: CGFloat = 16
    /// Between sections of a page.
    static let xl: CGFloat = 24
}

enum HorusStyle {
    static let bodyFont: Font = .body
    static let controlFont: Font = .body.weight(.medium)
    static let metadataFont: Font = .footnote.monospaced()
    static let badgeFont: Font = .footnote.weight(.medium)
    /// The title of a section or a card.
    static let titleFont: Font = .headline
    /// Prose one step under the body: a note under a control, a label over a figure.
    /// `metadataFont` is the monospaced twin, for values rather than sentences.
    static let captionFont: Font = .footnote
    static let cardRadius: CGFloat = 22
    static let controlRadius: CGFloat = 9
    /// Between a control and a card: the radius a card keeps when it shrinks to a tile.
    static let tileRadius: CGFloat = 14
    static let cardShape = RoundedRectangle(cornerRadius: cardRadius, style: .continuous)
    static let controlShape = RoundedRectangle(cornerRadius: controlRadius, style: .continuous)
    static let tileShape = RoundedRectangle(cornerRadius: tileRadius, style: .continuous)
    static let cardPadding: CGFloat = 14

    // MARK: Rows
    /// Minimum height of a row, by how much it has to carry. A row that can be tapped needs
    /// the full target; the two below it are for rows that only read.
    static let rowCompact: CGFloat = 26
    static let rowRegular: CGFloat = 30
    static let rowTouch: CGFloat = 44
    static let badgeHeight = rowCompact
    static let controlHeight = rowRegular
    static let iconButtonSize = rowTouch

    // MARK: Glyphs
    /// Marks that qualify a row rather than name it: carets, disclosure, trailing hints.
    static let glyphMark: CGFloat = 11
    /// A glyph standing beside text as the subject of the row.
    static let glyphInline: CGFloat = 14
    /// The leading mark of a header, and the standalone controls in the composer.
    static let glyphLead: CGFloat = 18
    /// Glyphs sit inside a 44pt target, so 16 left them floating in air. This fills the
    /// button without changing it: the tap area, and every explicit size a call site asks
    /// for, are untouched.
    static let iconSize: CGFloat = 22
    /// The column every inline glyph is centred in, so the text beside it starts at the same
    /// x on every row whatever the glyph's own size. Anything larger keeps its own width.
    ///
    /// Above the scale there is no token: a hero glyph on a card or an empty state is sized
    /// to its container, not to the text beside it, so those stay literals at the call site.
    static let glyphGutter: CGFloat = 18

    static let borderWidth: CGFloat = 0.75
    /// Empty space an icon button keeps around its glyph to reach a full tap target.
    static let iconButtonInset = (iconButtonSize - iconSize) / 2
    /// Outer padding for a row of icon buttons: they carry `iconButtonInset` of their own,
    /// so matching the margin of neighbouring text means subtracting it here.
    static let iconRowPadding = cardPadding + HorusSpace.xs - iconButtonInset
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
    static let arrowUp02 = Self("hi.arrowUp02")
    static let arrowUpRight01 = Self("hi.arrowUpRight01")
    static let aiSecurity02 = Self("hi.aiSecurity02")
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
    static let circle = Self("hi.circle")
    static let circleDot = Self("hi.circleDot")
    static let circleDotDashed = Self("hi.circleDotDashed")
    static let clock = Self("hi.clock")
    static let combine = Self("hi.combine")
    static let copy = Self("hi.copy")
    static let cpu = Self("hi.cpu")
    static let csv = Self("hi.csv")
    static let deepseek = Self("hi.deepseek")
    static let doc = Self("hi.doc")
    static let dotsThree = Self("hi.dotsThree")
    static let fileAxisThreeD = Self("hi.fileAxisThreeD")
    static let fileMagnifyingGlass = Self("hi.fileMagnifyingGlass")
    static let fileScript = Self("hi.fileScript")
    static let fileText = Self("hi.fileText")
    static let fileUpload = Self("hi.fileUpload")
    static let fingerprint = Self("hi.fingerprint")
    static let floppyDisk = Self("hi.floppyDisk")
    static let folder = Self("hi.folder")
    static let folderPlus = Self("hi.folderPlus")
    static let gear = Self("hi.gear")
    static let gitBranch = Self("hi.gitBranch")
    static let globe02 = Self("hi.globe02")
    static let go = Self("hi.go")
    static let group01 = Self("hi.group01")
    static let hardDrives = Self("hi.hardDrives")
    static let image01 = Self("hi.image01")
    static let info = Self("hi.info")
    static let javaScript = Self("hi.javaScript")
    static let key = Self("hi.key")
    static let kimiAi = Self("hi.kimiAi")
    static let link = Self("hi.link")
    static let loading02 = Self("hi.loading02")
    static let lockOpen = Self("hi.lockOpen")
    static let magnifyingGlass = Self("hi.magnifyingGlass")
    static let markdown = Self("hi.markdown")
    static let menu = Self("hi.menu")
    static let mic01 = Self("hi.mic01")
    static let moon = Self("hi.moon")
    static let neuralNetwork = Self("hi.neuralNetwork")
    static let note01 = Self("hi.note01")
    static let notePencil = Self("hi.notePencil")
    static let notificationSquare = Self("hi.notificationSquare")
    static let path = Self("hi.path")
    static let pencilSimple = Self("hi.pencilSimple")
    static let playFill = Self("hi.playFill")
    static let plugsConnected = Self("hi.plugsConnected")
    static let plus = Self("hi.plus")
    static let python = Self("hi.python")
    static let pushPin = Self("hi.pushPin")
    static let pushPinSlash = Self("hi.pushPinSlash")
    static let question = Self("hi.question")
    static let robot = Self("hi.robot")
    static let rust = Self("hi.rust")
    static let saveAll = Self("hi.saveAll")
    static let sealCheck = Self("hi.sealCheck")
    static let shield02 = Self("hi.shield02")
    static let shieldAlert = Self("hi.shieldAlert")
    static let shieldCheck = Self("hi.shieldCheck")
    static let shieldOff = Self("hi.shieldOff")
    static let sidebarSimple = Self("hi.sidebarSimple")
    static let signIn = Self("hi.signIn")
    static let slidersHorizontal = Self("hi.slidersHorizontal")
    static let setup01 = Self("hi.setup01")
    static let sparkle = Self("hi.sparkle")
    static let squaresFour = Self("hi.squaresFour")
    static let stopFill = Self("hi.stopFill")
    static let terminalWindow = Self("hi.terminalWindow")
    static let text = Self("hi.text")
    static let trash = Self("hi.trash")
    static let typeCursor = Self("hi.typeCursor")
    static let typeScript = Self("hi.typeScript")
    static let userFocus = Self("hi.userFocus")
    static let volumeHigh = Self("hi.volumeHigh")
    static let warning = Self("hi.warning")
    static let warningOctagon = Self("hi.warningOctagon")
    static let x = Self("hi.x")
    static let xCircle = Self("hi.xCircle")
}

struct HorusIcon: View {
    let glyph: HorusGlyph
    var size = HorusStyle.iconSize
    var foreground: Color? = nil
    /// Off for a glyph inside a capsule, where the column's slack reads as a gap in the
    /// pill rather than as a column shared with the rows above and below.
    var gutter = true

    init(
        _ glyph: HorusGlyph,
        size: CGFloat = HorusStyle.iconSize,
        foreground: Color? = nil,
        gutter: Bool = true
    ) {
        self.glyph = glyph
        self.size = size
        self.foreground = foreground
        self.gutter = gutter
    }

    @ViewBuilder
    var body: some View {
        // The asset carries `template-rendering-intent`, so this tints from the foreground
        // style the way a symbol does. Unlike a symbol it has no intrinsic text size, which is
        // why every glyph is drawn into an explicit square instead of following the font.
        let column = gutter ? max(size, HorusStyle.glyphGutter) : size
        let icon = Image(glyph.asset)
            .renderingMode(.template)
            .resizable()
            .aspectRatio(contentMode: .fit)
            .frame(width: size, height: size)
            // Centred in a fixed column: a 11pt caret and a 18pt file mark then leave the
            // text beside them starting at the same x, which is what makes a list of rows
            // read as a list rather than as a stack of near misses.
            .frame(width: column, height: column)
            .accessibilityHidden(true)
        if let foreground {
            icon.foregroundStyle(foreground)
        } else {
            icon
        }
    }
}

/// A band of light travelling across a label for as long as the work behind it is running.
///
/// Built like the spinner and for the same reason: the phase comes off the clock rather than
/// a `repeatForever` animation, because a streaming turn rebuilds these rows constantly and
/// a repeating animation restarts — visibly stuttering — on every rebuild. The band is
/// masked by the content, so it lights the glyphs rather than a rectangle around them.
private struct HorusRunningShimmer: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    let active: Bool

    private static let period = 1.6

    func body(content: Content) -> some View {
        let paused = reduceMotion || scenePhase != .active
        if active, !paused {
            TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: false)) { _ in
                let phase = ProcessInfo.processInfo.systemUptime
                    .truncatingRemainder(dividingBy: Self.period) / Self.period
                // The row is dimmed and a full-strength copy of itself is revealed through a
                // travelling band. Painting light *over* the row instead does nothing here:
                // these labels are already near-white, and white on white is white.
                content
                    .opacity(0.5)
                    .overlay {
                        content
                            .mask {
                                GeometryReader { proxy in
                                    let travel = proxy.size.width + 200
                                    LinearGradient(
                                        colors: [.clear, .white, .white, .clear],
                                        startPoint: .leading,
                                        endPoint: .trailing
                                    )
                                    .frame(width: 120)
                                    .offset(x: CGFloat(phase) * travel - 100)
                                }
                            }
                            .allowsHitTesting(false)
                    }
            }
        } else if active {
            // Reduce Motion and inactive scenes retain a non-animated pending cue.
            content.opacity(0.5)
        } else {
            content
        }
    }
}

extension View {
    /// Runs a shimmer across this view while `active` is true.
    func horusRunningShimmer(active: Bool) -> some View {
        modifier(HorusRunningShimmer(active: active))
    }
}

/// A bright head chasing a fading tail around the app's accent-colored loading track.
///
/// The angle comes off the clock rather than an `onAppear` animation because streaming turns
/// rebuild these rows and would visibly restart a `repeatForever` animation.
struct HorusSpinner: View {
    @Environment(\.horusPalette) private var palette
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    var size = HorusStyle.iconSize
    var foreground: Color?

    var body: some View {
        let paused = reduceMotion || scenePhase != .active
        let tint = foreground ?? palette.accent
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: paused)) { _ in
            let turn = paused
                ? 0
                : ProcessInfo.processInfo.systemUptime
                    .truncatingRemainder(dividingBy: 0.9) / 0.9
            let ringSize = size * 1.04
            let lineWidth = max(1.8, size * 0.14)
            let headSize = max(2.4, size * 0.19)
            let radius = max(0, (ringSize - lineWidth) / 2)
            ZStack {
                Circle()
                    .trim(from: 0.06, to: 0.86)
                    .stroke(
                        AngularGradient(
                            gradient: Gradient(stops: [
                                .init(color: tint.opacity(0), location: 0.06),
                                .init(color: tint.opacity(0.04), location: 0.3),
                                .init(color: tint.opacity(0.14), location: 0.52),
                                .init(color: tint.opacity(0.38), location: 0.72),
                                .init(color: tint.opacity(0.82), location: 0.86),
                            ]),
                            center: .center
                        ),
                        style: StrokeStyle(lineWidth: lineWidth, lineCap: .round)
                    )
                    .rotationEffect(.degrees(turn * 360 - 90))
                Circle()
                    .fill(tint)
                    .frame(width: headSize, height: headSize)
                    .offset(y: -radius)
                    .rotationEffect(.degrees(turn * 360 + 0.86 * 360))
            }
            .frame(width: ringSize, height: ringSize)
            .frame(width: size, height: size)
        }
        .accessibilityHidden(true)
    }
}

/// Reveals the already-laid-out title glyph by glyph. Keeping the complete `Text` in the
/// layout avoids resizing the toolbar and sidebar on every animation frame.
private struct HorusTitleTypingRenderer: TextRenderer {
    var progress: Double
    var showsCursor: Bool
    var cursorColor: Color

    var animatableData: Double {
        get { progress }
        set { progress = newValue }
    }

    func draw(layout: Text.Layout, in context: inout GraphicsContext) {
        let slices = layout.flatMap { line in line.flatMap { run in run } }
        let revealed = min(max(progress, 0), 1) * Double(slices.count)
        for (index, slice) in slices.enumerated() {
            let opacity = min(max(revealed - Double(index), 0), 1)
            guard opacity > 0 else { continue }
            var copy = context
            copy.opacity = opacity
            copy.draw(slice)
        }

        guard showsCursor, let line = layout.first else { return }
        let visibleIndex = min(Int(ceil(revealed)), slices.count) - 1
        let cursor: (x: CGFloat, baseline: CGFloat, ascent: CGFloat, descent: CGFloat)
        if visibleIndex >= 0 {
            let bounds = slices[visibleIndex].typographicBounds
            cursor = (
                bounds.origin.x + bounds.width,
                bounds.origin.y,
                bounds.ascent,
                bounds.descent
            )
        } else {
            let bounds = line.typographicBounds
            cursor = (line.origin.x, line.origin.y, bounds.ascent, bounds.descent)
        }
        var path = Path()
        path.move(to: CGPoint(x: cursor.x, y: cursor.baseline - cursor.ascent))
        path.addLine(to: CGPoint(x: cursor.x, y: cursor.baseline + cursor.descent))
        context.stroke(path, with: .color(cursorColor), lineWidth: 1.25)
    }
}

private enum HorusTitleTypingPhase {
    case settled
    case erasing
    case typing
}

private struct HorusTitleTypingRequest: Equatable {
    let title: String
    let reduceMotion: Bool
}

struct HorusTitleText: View {
    private static let eraseDuration: TimeInterval = 0.36
    private static let typingDuration: TimeInterval = 0.6

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let title: String
    let cursorColor: Color
    @State private var displayedTitle: String
    @State private var progress = 1.0
    @State private var phase = HorusTitleTypingPhase.settled

    init(title: String, cursorColor: Color = .primary) {
        self.title = title
        self.cursorColor = cursorColor
        _displayedTitle = State(initialValue: title)
    }

    var body: some View {
        Text(displayedTitle)
            .textRenderer(HorusTitleTypingRenderer(
                progress: progress,
                showsCursor: phase != .settled,
                cursorColor: cursorColor
            ))
            .task(id: HorusTitleTypingRequest(title: title, reduceMotion: reduceMotion)) {
                await animateTitleChange()
            }
            .accessibilityRepresentation { Text(title) }
    }

    @MainActor
    private func animateTitleChange() async {
        guard displayedTitle != title else {
            progress = 1
            phase = .settled
            return
        }
        guard !reduceMotion else {
            displayedTitle = title
            progress = 1
            phase = .settled
            return
        }

        do {
            phase = .erasing
            withAnimation(.linear(duration: Self.eraseDuration)) { progress = 0 }
            try await Task.sleep(for: .seconds(Self.eraseDuration))

            displayedTitle = title
            progress = 0
            await Task.yield()
            try Task.checkCancellation()

            phase = .typing
            withAnimation(.linear(duration: Self.typingDuration)) { progress = 1 }
            try await Task.sleep(for: .seconds(Self.typingDuration))
            try Task.checkCancellation()
            phase = .settled
        } catch is CancellationError {
            // A newer title owns the next animation phase.
        } catch {
            displayedTitle = title
            progress = 1
            phase = .settled
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

/// Row of capsule actions. Rows of several actions drop their labels on a narrow
/// screen; a single action keeps its label and goes full width instead.
struct HorusActionRow<Content: View>: View {
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    var collapsesToIcons = false
    @ViewBuilder let content: Content

    var body: some View {
        Group {
            if iconsOnly {
                HStack(spacing: HorusSpace.s) { content }
                    .labelStyle(.iconOnly)
                    .buttonBorderShape(.circle)
            } else {
                // Full width keeps the label: measuring for a horizontal fit either
                // hyphenates or truncates it away.
                VStack(spacing: HorusSpace.s) { content }.buttonSizing(.flexible)
            }
        }
        .frame(maxWidth: .infinity)
        .lineLimit(1)
        .buttonStyle(.horusGlass)
        .buttonBorderShape(iconsOnly ? .circle : .capsule)
        .controlSize(.large)
    }

    private var iconsOnly: Bool {
        collapsesToIcons && horizontalSizeClass == .compact
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
/// this table is the iOS client's half of that contract: one entry per `FrontendSymbol`
/// variant, and the gateway never names an icon itself. Keep it in step with the enum in
/// `src/protocol/mod.rs` — a variant with no entry here falls back to `placeholder`.
///
/// `FrontendSymbol::Custom` has no entry by definition. It carries a name from outside the
/// protocol's vocabulary, which this app has no artwork for, so it draws the placeholder.
enum HorusSymbol {
    private static let placeholder = HorusGlyph.question

    static func glyph(for symbol: String) -> HorusGlyph {
        vocabulary[symbol] ?? placeholder
    }

    /// Nil where `glyph(for:)` would return the placeholder. Beside a label the placeholder
    /// reads as a broken glyph rather than a neutral one, so a caller can drop it instead.
    static func knownGlyph(for symbol: String) -> HorusGlyph? {
        vocabulary[symbol]
    }

    /// One entry per `FrontendSymbol` variant.
    private static let vocabulary: [String: HorusGlyph] = [
        "agent": .robot,
        "brain": .brain,
        "branch": .gitBranch,
        "chat": .chatCircle,
        "chat_gpt": .chatGpt,
        "claude": .claude,
        "deepseek": .deepseek,
        "delete": .trash,
        "edit": .pencilSimple,
        "kimi": .kimiAi,
        "moon": .moon,
        "promote": .arrowCircleUp,
        "route": .path,
        "search": .magnifyingGlass,
        "sparkle": .sparkle,
        "storage": .hardDrives,
        "task": .checkCircle,
    ]
}

struct HorusPalette: Sendable {
    let canvas: Color
    /// Base surface behind the compact drawer and embedded document views.
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
    let sidebarScrim: Color

    // Keep the Nord surface steps distinct: chat bubbles, tool details, and diff rows rely
    // on this hierarchy instead of carrying one-off borders and backgrounds.
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
            sidebarScrim = .nord3
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
            sidebarScrim = .nord6
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
    static let nord0 = Color(red: 46.0 / 255.0, green: 52.0 / 255.0, blue: 64.0 / 255.0)
    static let nord1 = Color(red: 0.231, green: 0.259, blue: 0.322)
    static let nord2 = Color(red: 0.263, green: 0.298, blue: 0.369)
    static let nord3 = Color(red: 76.0 / 255.0, green: 86.0 / 255.0, blue: 106.0 / 255.0)
    static let nord4 = Color(red: 216.0 / 255.0, green: 222.0 / 255.0, blue: 233.0 / 255.0)
    static let nord5 = Color(red: 0.898, green: 0.914, blue: 0.941)
    static let nord6 = Color(red: 236.0 / 255.0, green: 239.0 / 255.0, blue: 244.0 / 255.0)
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
        HStack(spacing: HorusSpace.s) {
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
                HorusIcon(glyph, size: HorusStyle.glyphInline, foreground: foreground, gutter: false)
            }
            if !text.isEmpty { Text(text).lineLimit(1) }
        }
        .font(HorusStyle.badgeFont)
        .foregroundStyle(foreground)
        .padding(.horizontal, HorusSpace.m)
        .frame(height: HorusStyle.badgeHeight)
        .horusGlass(in: Capsule(), interactive: interactive)
    }

    private var foreground: Color { palette.tone(tone) }
}

/// A menu's current value: the provider's mark, the value itself, and a muted qualifier.
/// No container — the icon buttons it sits beside carry none either, and the hierarchy
/// between the three parts is what separates it from the row.
struct HorusMenuLabel: View {
    @Environment(\.horusPalette) private var palette
    let text: String
    var glyph: HorusGlyph?
    var detail: String?
    var showsDisclosure = true
    /// The composer sizes its mark up to `glyphLead` to sit level with the icon buttons
    /// beside it; inline in a header the glyph stays on the text's own step.
    var glyphSize = HorusStyle.glyphInline
    /// A settings row reads at body size beside its label; the composer and the file header
    /// carry the badge step so the label sits under the text it belongs to.
    var font = HorusStyle.badgeFont

    var body: some View {
        HStack(spacing: HorusSpace.s) {
            if let glyph {
                HorusIcon(glyph, size: glyphSize, foreground: palette.accent, gutter: false)
            }
            Text(text)
                .font(font)
                .lineLimit(1)
                .truncationMode(.middle)
            if let detail {
                Text(detail)
                    .font(font)
                    .foregroundStyle(palette.muted)
                    .lineLimit(1)
            }
            if showsDisclosure {
                HorusIcon(.caretUpDown, size: HorusStyle.glyphMark, foreground: palette.muted, gutter: false)
            }
        }
        .frame(minHeight: HorusStyle.controlHeight)
        .contentShape(Rectangle())
    }
}

struct HorusFeedbackButtonStyle<Base: PrimitiveButtonStyle>: PrimitiveButtonStyle {
    let base: Base

    @ViewBuilder
    func makeBody(configuration: Configuration) -> some View {
        FeedbackButton(configuration: configuration, base: base)
    }

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
    /// Draws only the glyph — no glass circle behind it — while keeping the full
    /// 44pt hit target. `prominent` then tints the glyph instead of the fill.
    var bare = false

    func makeBody(configuration: Configuration) -> some View {
        IconButton(
            label: configuration.label,
            isPressed: configuration.isPressed,
            prominent: prominent,
            bare: bare
        )
    }

    private struct IconButton: View {
        @Environment(\.horusPalette) private var palette
        // A custom style gets no automatic disabled treatment: without this the send button
        // keeps a full-strength accent glyph on a circle that no longer responds.
        @Environment(\.isEnabled) private var isEnabled
        let label: ButtonStyleConfiguration.Label
        let isPressed: Bool
        let prominent: Bool
        let bare: Bool

        var body: some View {
            let base = label
                .font(HorusStyle.controlFont)
                .foregroundStyle(foreground)
                .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                // A glass effect adds no hit area, so without this the tap target is the
                // 16pt glyph, not the 44pt circle: unusable by touch, fine with a cursor.
                .contentShape(Circle())
            Group {
                if bare {
                    base
                } else {
                    base.horusGlass(in: Circle(), interactive: isEnabled, prominent: prominent && isEnabled)
                }
            }
            .opacity(isPressed ? 0.72 : 1)
            .sensoryFeedback(.impact(weight: .light), trigger: isPressed) { _, pressed in pressed }
        }

        private var foreground: Color {
            guard isEnabled else { return palette.muted }
            if prominent { return bare ? palette.accent : palette.onAccent }
            return .primary
        }
    }
}

/// A prominent button with a label that stays legible on the accent in both schemes.
private struct HorusProminentButton: ViewModifier {
    @Environment(\.horusPalette) private var palette

    func body(content: Content) -> some View {
        // `.glassProminent` fills from the tint, so it needs the accessible one rather than
        // the global tint `HorusTheme` sets for switches, pickers, and links.
        content
            .buttonStyle(.horusGlassProminent)
            .tint(palette.accentFill)
            .foregroundStyle(palette.onAccent)
    }
}

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
        VStack(alignment: .leading, spacing: HorusSpace.s) {
            Text(title)
                .font(HorusStyle.titleFont)
            Text(detail)
                .font(HorusStyle.bodyFont)
                .foregroundStyle(palette.muted)
        }
    }
}

extension View {
    func horusTheme() -> some View { modifier(HorusTheme()) }
}
