import SwiftUI

struct AppLockView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    @ScaledMetric(relativeTo: .largeTitle) private var iconSize: CGFloat = 72

    var body: some View {
        ZStack {
            HorusBackdrop()
            Button {
                Task { await model.unlockApp() }
            } label: {
                HorusIcon(
                    model.appLockError == nil
                        ? model.appLockAuthenticationMethod.glyph
                        : .warningOctagon,
                    size: iconSize,
                    foreground: model.appLockError == nil ? palette.accent : palette.danger
                )
                .frame(width: 128, height: 128)
                .contentShape(Circle())
            }
            .buttonStyle(.horusPlain)
            .disabled(model.isAppLockAuthenticating)
            .opacity(model.isAppLockAuthenticating ? 0.45 : 1)
            .accessibilityLabel(
                model.appLockError == nil
                    ? model.appLockAuthenticationMethod.unlockTitle
                    : "Try Again"
            )
            .accessibilityValue(
                model.isAppLockAuthenticating
                    ? "Authenticating"
                    : model.appLockError ?? "Horus is locked"
            )
        }
    }
}

struct AppToastOverlay: View {
    @Environment(AppModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        ZStack {
            if let toast = model.toast {
                AppToastView(toast: toast, dismiss: dismiss)
                    .transition(
                        reduceMotion
                            ? .opacity
                            : .move(edge: .top).combined(with: .opacity)
                    )
            }
        }
        .frame(maxWidth: 520)
        .padding(.horizontal, HorusSpace.l)
        .padding(.top, HorusSpace.m)
        .allowsHitTesting(model.toast != nil)
        .animation(toastAnimation, value: model.toast?.id)
    }

    private var toastAnimation: Animation {
        reduceMotion ? .easeOut(duration: 0.12) : .smooth(duration: 0.28)
    }

    private func dismiss() {
        withAnimation(toastAnimation) { model.dismissToast() }
    }
}

private struct AppToastView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let toast: AppToast
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: HorusSpace.m) {
            if let sessionID = toast.sessionID {
                Button {
                    model.showsInspector = false
                    model.showsPairing = false
                    model.showsWorkspaceBrowser = false
                    model.openChat(sessionID)
                    dismiss()
                } label: {
                    toastMessage
                }
                .buttonStyle(.horusPlain)
                .accessibilityLabel(accessibilityLabel)
                .accessibilityHint("Opens this chat")
            } else {
                toastMessage
                    .accessibilityElement(children: .ignore)
                    .accessibilityLabel(accessibilityLabel)
            }

            Button(action: dismiss) {
                HorusIcon(.x, size: HorusStyle.glyphInline, foreground: palette.muted)
                    .frame(width: HorusStyle.iconButtonSize, height: HorusStyle.iconButtonSize)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.horusPlain)
            .accessibilityLabel("Dismiss notification")
        }
        .padding(.leading, HorusSpace.l)
        .padding(.trailing, HorusSpace.s)
        .padding(.vertical, HorusSpace.m)
        .horusGlass(in: HorusStyle.cardShape, interactive: true)
        .shadow(color: .black.opacity(0.20), radius: 18, y: 8)
        .gesture(
            DragGesture(minimumDistance: 20)
                .onEnded { value in
                    guard value.predictedEndTranslation.height < -40 else { return }
                    dismiss()
                }
        )
    }

    private var toastMessage: some View {
        HStack(alignment: .top, spacing: HorusSpace.m) {
            HorusIcon(
                toast.tone.glyph,
                size: 18,
                foreground: toast.tone.color(in: palette)
            )
            VStack(alignment: .leading, spacing: HorusSpace.xxs) {
                Text(toast.tone.title)
                    .font(HorusStyle.controlFont.weight(.semibold))
                    .foregroundStyle(toast.tone.color(in: palette))
                Text(toast.message)
                    .font(HorusStyle.bodyFont)
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    private var accessibilityLabel: String {
        "\(toast.tone.title): \(toast.message)"
    }
}

extension ToastTone {
    var title: String {
        switch self {
        case .info: "Notice"
        case .success: "Done"
        case .warning: "Attention"
        case .error: "Error"
        }
    }

    var glyph: HorusGlyph {
        switch self {
        case .info: .info
        case .success: .checkCircle
        case .warning: .warning
        case .error: .xCircle
        }
    }

    func color(in palette: HorusPalette) -> Color {
        switch self {
        case .info: palette.accent
        case .success: palette.signal
        case .warning: palette.warning
        case .error: palette.danger
        }
    }
}
