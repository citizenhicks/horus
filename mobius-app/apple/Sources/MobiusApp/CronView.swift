import SwiftUI

struct CronView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette

    var body: some View {
        // Creating a schedule needs a live chat, so that action lives in the chat menu.
        PageScaffold(
            title: "Schedules",
            detail: "Run durable möbius tasks on the gateway workspace, even when this app is closed."
        ) {
            if let error = model.cronError {
                StatusBanner(tone: .error, title: "Schedule rejected", detail: error)
            }
            if !model.isSchedulingEnabled {
                DisabledCapabilityNotice(
                    title: "Scheduling is off",
                    detail: "Saved tasks and run history remain visible. Enable Cron in this chat to change or run them."
                )
            }

            Section {
                if model.cronTasks.isEmpty {
                    Text("No scheduled tasks yet.").foregroundStyle(palette.muted)
                }
                ForEach(model.cronTasks) { task in
                    CronTaskRow(task: task)
                }
            } header: {
                HStack {
                    Text("Tasks")
                    Spacer()
                    Button("Refresh", glyph: .arrowClockwise) { model.refreshCron() }
                        .labelStyle(.iconOnly)
                        .buttonStyle(MobiusIconButtonStyle())
                        .help("Refresh schedules")
                }
            }

            Section("Run history") {
                if model.cronRuns.isEmpty {
                    Text("No scheduled runs yet.").foregroundStyle(palette.muted)
                }
                ForEach(model.cronRuns) { run in
                    CronRunRow(run: run)
                }
            }
        }
        .task { if model.connectionState.isReady { model.refreshCron() } }
    }
}

private struct CronTaskRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    let task: CronTask
    @State private var schedule: String

    init(task: CronTask) {
        self.task = task
        _schedule = State(initialValue: task.schedule)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: MobiusSpace.m) {
            VStack(alignment: .leading, spacing: MobiusSpace.xs) {
                Text(task.task)
                    .font(MobiusStyle.bodyFont.weight(.semibold))
                    .textSelection(.enabled)
                Text("ID \(task.id)")
                    .font(MobiusStyle.metadataFont)
                    .foregroundStyle(palette.muted)
            }
            LabeledContent("Schedule") {
                TextField("* * * * *", text: $schedule)
                    .font(MobiusStyle.bodyFont.monospaced())
                    .settingsField()
                    .disabled(!model.isSchedulingEnabled)
            }
            MobiusActionRow(collapsesToIcons: true) {
                Button("Run now", glyph: .playFill) { model.runCron(task) }
                    .mobiusProminentButton()
                Button("Reschedule", glyph: .clock) {
                    model.rescheduleCron(task, schedule: schedule)
                }
                    .disabled(schedule == task.schedule)
                Button("Delete", glyph: .trash, role: .destructive) {
                    model.deleteCron(task)
                }
            }
            .disabled(!model.isSchedulingEnabled)
        }
        .onChange(of: task.schedule) { schedule = task.schedule }
    }
}

private struct CronRunRow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.mobiusPalette) private var palette
    let run: CronRun

    var body: some View {
        HStack(alignment: .top, spacing: MobiusSpace.m) {
            Circle().fill(statusColor).frame(width: 9, height: 9).padding(.top, MobiusSpace.xs)
            VStack(alignment: .leading, spacing: MobiusSpace.xs) {
                HStack {
                    Text(run.status.rawValue.uppercased())
                        .font(MobiusStyle.metadataFont.weight(.bold))
                    Text(Date(timeIntervalSince1970: TimeInterval(run.startedAt)), style: .relative)
                        .font(MobiusStyle.bodyFont)
                        .foregroundStyle(palette.muted)
                }
                Text("Task \(run.taskId)").font(MobiusStyle.metadataFont)
                if let message = run.message {
                    Text(message).font(MobiusStyle.bodyFont).foregroundStyle(palette.muted)
                }
                if let sessionID = run.sessionId {
                    Button("Open session") {
                        model.openChat(sessionID)
                    }
                    .buttonStyle(.mobiusGlass)
                    .buttonBorderShape(.capsule)
                    .disabled(!model.canOpenSession && sessionID != model.selectedSessionID)
                    .padding(.top, MobiusSpace.xxs)
                }
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(run.status.rawValue) run for task \(run.taskId)")
    }

    private var statusColor: Color {
        switch run.status {
        case .succeeded: palette.signal
        case .failed: palette.danger
        case .running: palette.accent
        case .skipped: palette.muted
        }
    }
}


/// A compact HugeIcons disclosure for setting guidance that should not permanently occupy a row.
