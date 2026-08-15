import SwiftUI

/// Model and reasoning as two rows, in the composer's glyph-led menu style.
///
/// The gateway advertises one route per model-and-effort pair, so one combined list
/// multiplies every model by every effort and buries the choice that matters. Split, each
/// list stays short and the effort reads as its own decision. The reasoning row appears only
/// when the chosen model actually offers more than one.
struct ModelRoutePicker: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horusPalette) private var palette
    let label: String
    let detail: String
    let choices: [ModelChoice]
    var unsetLabel: String?
    var isEnabled = true
    @Binding var route: String?

    var body: some View {
        LabeledContent {
            Menu {
                Picker(label, selection: modelSelection) {
                    if let unsetLabel {
                        Text(unsetLabel).tag(String?.none)
                    }
                    ForEach(distinctModels, id: \.route) { choice in
                        optionLabel(
                            model.modelLabel(for: choice),
                            symbol: model.providerSymbol(for: choice)
                        )
                        .tag(Optional(choice.route))
                    }
                }
                .labelsHidden()
            } label: {
                menuLabel(selectedModelLabel, glyph: selectedGlyph)
            }
            .menuIndicator(.hidden)
            .buttonStyle(.horusPlain)
            .disabled(!isEnabled)
            .accessibilityLabel(label)
            .accessibilityValue(selectedModelLabel)
        } label: {
            HStack(spacing: HorusSpace.xs) {
                Text(label)
                SettingsInfoButton(title: label, detail: detail)
            }
        }
        .sensoryFeedback(.selection, trigger: route)

        if reasoningChoices.count > 1 {
            LabeledContent("Reasoning") {
                Menu {
                    Picker("Reasoning", selection: reasoningSelection) {
                        ForEach(reasoningChoices, id: \.route) { choice in
                            Text(effortLabel(choice)).tag(choice.route)
                        }
                    }
                    .labelsHidden()
                } label: {
                    menuLabel(selected.map(effortLabel) ?? "Default", glyph: nil)
                }
                .menuIndicator(.hidden)
                .buttonStyle(.horusPlain)
                .disabled(!isEnabled)
                .accessibilityLabel("Reasoning")
                .accessibilityValue(selected.map(effortLabel) ?? "Default")
            }
        }
    }

    private func menuLabel(_ text: String, glyph: HorusGlyph?) -> some View {
        HorusMenuLabel(text: text, glyph: glyph, font: HorusStyle.bodyFont)
            .foregroundStyle(palette.accent)
    }

    @ViewBuilder
    private func optionLabel(_ title: String, symbol: String?) -> some View {
        if let symbol, let glyph = HorusSymbol.knownGlyph(for: symbol) {
            HorusLabel(title: title, glyph: glyph)
        } else {
            Text(title)
        }
    }

    private var selected: ModelChoice? {
        choices.first { $0.route == route }
    }

    private var selectedModelLabel: String {
        guard let selected else { return unsetLabel ?? "Select" }
        return model.modelLabel(for: selected)
    }

    private var selectedGlyph: HorusGlyph? {
        selected
            .flatMap { model.providerSymbol(for: $0) }
            .flatMap { HorusSymbol.knownGlyph(for: $0) }
    }

    private func effortLabel(_ choice: ModelChoice) -> String {
        choice.reasoningEffort?.capitalized ?? "Default"
    }

    private var distinctModels: [ModelChoice] {
        var seen = Set<String>()
        return choices.filter { seen.insert("\($0.group)\u{0}\($0.model)").inserted }
    }

    private var reasoningChoices: [ModelChoice] {
        guard let selected else { return [] }
        return choices.filter { $0.group == selected.group && $0.model == selected.model }
    }

    /// Switching model keeps the effort when the new model offers the same one, so changing
    /// model does not silently reset reasoning to the provider default.
    private var modelSelection: Binding<String?> {
        Binding {
            guard let selected else { return nil }
            return distinctModels.first {
                $0.group == selected.group && $0.model == selected.model
            }?.route ?? selected.route
        } set: { newRoute in
            guard let newRoute, let choice = choices.first(where: { $0.route == newRoute }) else {
                route = nil
                return
            }
            let effort = selected?.reasoningEffort
            route = choices.first {
                $0.group == choice.group
                    && $0.model == choice.model
                    && $0.reasoningEffort == effort
            }?.route ?? choice.route
        }
    }

    private var reasoningSelection: Binding<String> {
        Binding { route ?? "" } set: { route = $0 }
    }
}
