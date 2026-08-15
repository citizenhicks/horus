import SwiftUI

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
