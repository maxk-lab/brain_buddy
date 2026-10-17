import BrainBuddyAgents
import BrainBuddyAPI
import BrainBuddyCore
import BrainBuddyDesign
import BrainBuddyMacCore
import BrainBuddyWorkspace
import SwiftUI

extension AgentRunBlock {
    var color: CarryOnColorStyle {
        switch tone {
        case .plain: .coTextPrimary
        case .accent: .coTextAccent
        case .warning: .coStatusWarning
        case .danger: .coStatusDanger
        }
    }
}

/// The workspace's complete and create commands, for the step this inspector shows (030-FR-012, 030-FR-022).
@MainActor
struct InspectorStepCommands: StepCommands {
    let model: BrainBuddyModel
    let taskID: TaskID

    func completeStep(serverID: String) async throws {
        try await model.workspace.completeTask(taskID, editorID: UUID().uuidString)
    }

    func createNextAction(title: String, projectID: String?) async throws {
        let project = projectID.map { ProjectID($0) }
        let command: GTDCommand = .createTask(.init(taskID: .random(), title: title, list: .next, projectID: project))
        try await model.workspace.apply([command], editorID: UUID().uuidString)
    }
}

/// G-05: the latest run of the step in the inspector (030-FR-019 – 030-FR-028). Agent text is `Text(verbatim:)`:
/// no Markdown, no HTML, no link.
struct AgentRunSectionView: View {
    let model: BrainBuddyModel
    let task: TaskRecord
    /// "Hand off again" shows only when this is set (the hand-off sheet, 030 PR-09).
    var onHandOff: (() -> Void)?
    @State private var section: AgentRunSectionModel
    @FocusState private var runFocused: Bool
    @FocusState private var editorFocused: String?
    @State private var loading = true

    init?(model: BrainBuddyModel, task: TaskRecord, onHandOff: (() -> Void)? = nil) {
        guard let server = task.serverID, let account = model.workspace.account else { return nil }
        let client = BrainBuddyAPIClient(baseURL: account.serverURL, tokenStore: WorkspaceHost.systemTokenStore(), identity: WorkspaceHost.clientIdentity)
        let runs = model.agentRuns
        (self.model, self.task, self.onHandOff) = (model, task, onHandOff)
        _section = State(initialValue: AgentRunSectionModel(
            taskServerID: server, projectID: task.projectID?.rawValue, remote: client, availability: model.agentAvailability,
            steps: InspectorStepCommands(model: model, taskID: task.id), refreshStore: { await runs.refresh() },
            cachedRun: { [server] in runs.cachedRun(taskServerID: server) }))
    }

    /// Actions need a connection and a fresh read of this run; a stale view is read-only.
    private var online: Bool { model.agentAvailability.isOnline && !section.isStale }
    private var item: AgentRunItem? { model.agentRuns.items.first { $0.taskServerID == task.serverID } }
    private var refreshKey: String {
        "\(item?.state.label ?? "")|\(item?.lastContactAt?.timeIntervalSince1970 ?? 0)|\(model.agentRuns.lastUpdated?.timeIntervalSince1970 ?? 0)"
    }

    var body: some View {
        MacInspectorPanel(width: 380) {
            header
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if let run = section.shown {
                        runContent(run, block(for: run))
                    } else {
                        if loading { ProgressView().controlSize(.small).padding(.top, 22) }
                        MacInspectorMetaText(text: loading ? AgentsCopy.runsLoading : AgentsCopy.runsFailed)
                        if !loading {
                            Button(AgentsCopy.tryAgain) { Task { await load() } }.buttonStyle(.carryOnSecondary).padding(.top, 8)
                        }
                    }
                    earlierRuns
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
        } actionBar: {
            footer
        }
        .task(id: refreshKey) {
            await load()
            takeRequestedFocus()
        }
        .onAppear { takeRequestedFocus() }
        .onChange(of: model.agentRuns.inspectorFocusPending) { _, _ in takeRequestedFocus() }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 6) {
                if let projectID = task.projectID {
                    Circle().fill(Color(inspectorProjectDot(model.project(projectID)?.color))).frame(width: 8, height: 8)
                        .accessibilityHidden(true)
                    Text(verbatim: model.projectLabel(projectID))
                    Text(verbatim: "·")
                }
                Text(verbatim: section.shown?.agentName ?? item?.agentName ?? AgentsCopy.sidebar)
            }
            .font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
            .fixedSize(horizontal: false, vertical: true).accessibilityElement(children: .combine)
            Text(verbatim: task.title).font(MacRowFont.action(size: 20, weight: 600, relativeTo: .title2))
                .foregroundStyle(CarryOnColorStyle.coTextPrimary).fixedSize(horizontal: false, vertical: true)
                .padding(.top, 6).accessibilityAddTraits(.isHeader)
        }
    }

    private func block(for run: AgentRunDTO) -> AgentRunBlock {
        AgentRunBlock(run, now: section.now(), relay: model.agentAvailability.relayEnabled && onHandOff != nil)
    }

    private func load() async {
        loading = true
        await section.load()
        loading = false
    }

    /// Return on a run row asked for focus here (030-FR-040); the label is the first stop of the section.
    private func takeRequestedFocus() {
        if section.shown != nil, model.agentRuns.consumeInspectorFocus() { runFocused = true }
    }

    @ViewBuilder
    private func runContent(_ run: AgentRunDTO, _ block: AgentRunBlock) -> some View {
        Group {
            if block.showsResult || block.isExpired {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Circle().fill(CarryOnColorStyle.coMarkOpen).frame(width: 7, height: 7)
                    Text(verbatim: block.label).font(MacRowFont.meta).foregroundStyle(block.color)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.top, 14)
            } else {
                timeline(run, block)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityLabel(block.label).focusable().focused($runFocused)
        if !block.isExpired {
            if !online || model.agentRuns.phase == .stale, let at = model.agentRuns.lastUpdated ?? run.lastContactAt {
                MacInspectorMetaText(text: AgentsCopy.lastUpdated(at.formatted(date: .omitted, time: .shortened)))
            }
            stateBlock(run, block)
        }
    }

    private func timeline(_ run: AgentRunDTO, _ block: AgentRunBlock) -> some View {
        var rows = AgentTimelineRow.rows(run).map { (word: $0.word, meta: $0.meta) }
        if rows.last?.word != run.primaryState.label {
            rows.append((block.label, run.lastContactAt?.formatted(date: .omitted, time: .shortened) ?? ""))
        } else {
            rows[rows.count - 1].word = block.label
        }
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                let last = index == rows.count - 1
                HStack(alignment: .top, spacing: 10) {
                    VStack(spacing: 3) {
                        Circle().fill(last && block.tone != .plain ? block.color : .coMarkOpen).frame(width: 10, height: 10)
                        if !last { Rectangle().fill(CarryOnColorStyle.coMarkOpen.opacity(0.4)).frame(width: 1.5, height: 22) }
                    }
                    .frame(width: 10).padding(.top, 4).accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(verbatim: row.word).font(last ? MacRowFont.titleStrong : MacRowFont.title)
                            .foregroundStyle(CarryOnColorStyle.coTextPrimary)
                        Text(verbatim: row.meta).font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                }
                .frame(maxWidth: .infinity, minHeight: last ? 0 : 43, alignment: .topLeading)
                .accessibilityElement(children: .combine)
            }
        }
        .padding(.top, 18).padding(.bottom, 4)
    }

    @ViewBuilder
    private func stateBlock(_ run: AgentRunDTO, _ block: AgentRunBlock) -> some View {
        if let question = block.question {
            card(AgentsCopy.asks(run.agentName), question)
            if block.reply { replyForm(run) }
        }
        if let detail = block.detail { plain(detail, block.color) }
        if let contact = block.contact { MacInspectorMetaText(text: contact) }
        if block.showsResult { resultBlock(block) }
        if block.check { checkForm }
        if (block.reply || block.stop || block.check) && !online { MacInspectorMetaText(text: AgentsCopy.needsConnection) }
        if case .failed(let failure) = section.actions.phase {
            plain([failure.text(), failure.reference].compactMap { $0 }.joined(separator: " "), .coStatusDanger)
        }
    }

    private func plain(_ text: String, _ color: CarryOnColorStyle, topPadding: CGFloat = 8) -> some View {
        Text(verbatim: text).font(MacRowFont.action(size: 14, weight: 400, relativeTo: .body)).foregroundStyle(color)
            .fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, alignment: .leading).padding(.top, topPadding)
    }

    private func card(_ title: String, _ text: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(verbatim: title).font(MacRowFont.sectionHeader).foregroundStyle(CarryOnColorStyle.coTextSecondary)
            Text(verbatim: text).font(MacRowFont.action(size: 14, weight: 400, relativeTo: .body)).foregroundStyle(CarryOnColorStyle.coTextPrimary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(CarryOnColorStyle.coBgGround, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .padding(.top, 8).accessibilityElement(children: .combine)
    }

    private func field(_ name: String, text: Binding<String>, secure: Bool = false) -> some View {
        Group {
            if secure { SecureField(name, text: text) } else { TextField(name, text: text, axis: .vertical).lineLimit(1...5) }
        }
        .textFieldStyle(.plain).font(MacRowFont.title).foregroundStyle(CarryOnColorStyle.coTextPrimary)
        .padding(.horizontal, 10).padding(.vertical, 7)
        .focused($editorFocused, equals: name)
        .background(CarryOnColorStyle.coSurfaceCard, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(editorFocused == name ? CarryOnColorStyle.coTextPrimary : .coMarkOpen,
                              lineWidth: editorFocused == name ? 2 : 1)
        }
        .padding(.top, 8).accessibilityLabel(name)
    }

    private func replyForm(_ run: AgentRunDTO) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            field(AgentsCopy.yourAnswer, text: $section.answer)
            if run.replyPending { MacInspectorMetaText(text: AgentsCopy.replySent) }
        }
    }

    @ViewBuilder
    private var checkForm: some View {
        if section.asksPassword { field(AgentsCopy.currentPassword, text: $section.password, secure: true) }
        Button(AgentsCopy.checkAgain) { Task { await section.checkAgain() } }
            .buttonStyle(.carryOnSecondary).padding(.top, 8)
            .disabled(!online || section.actions.phase == .working || (section.asksPassword && section.password.isEmpty))
    }

    @ViewBuilder
    private func resultBlock(_ block: AgentRunBlock) -> some View {
        MacInspectorSectionLabel(AgentsCopy.result)
        VStack(alignment: .leading, spacing: 8) {
            if let result = block.result { plain(result, .coTextPrimary, topPadding: 0) }
            if let link = block.resultLink {
                Text(verbatim: link).font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
                    .fixedSize(horizontal: false, vertical: true).frame(maxWidth: .infinity, alignment: .leading)
                    .padding(8).background(CarryOnColorStyle.coControlQuiet, in: RoundedRectangle(cornerRadius: 8))
            }
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(CarryOnColorStyle.coBgGround, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        MacInspectorMetaText(text: AgentsCopy.finishingIsYours)
        MacInspectorSectionLabel(AgentsCopy.then)
        HStack(spacing: 10) {
            MacBoardIconView(icon: .nextActions, size: 18).foregroundStyle(CarryOnColorStyle.coTextPrimary)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: AgentsCopy.makeNextStep).font(MacRowFont.title).foregroundStyle(CarryOnColorStyle.coTextPrimary)
                TextField(AgentsCopy.makeNextStep, text: $section.nextStep, axis: .vertical).textFieldStyle(.plain)
                    .font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
                    .accessibilityLabel(AgentsCopy.makeNextStep)
            }
        }
        .padding(12).frame(maxWidth: .infinity, alignment: .leading)
        .background(CarryOnColorStyle.coBgGround, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }

    @ViewBuilder
    private var footer: some View {
        if let run = section.shown {
            let block = block(for: run)
            HStack(spacing: 8) {
                if block.reply {
                    Button(AgentsCopy.reply) { Task { await section.sendReply() } }
                        .buttonStyle(.carryOnPrimary)
                        .disabled(!online || run.replyPending || section.actions.phase == .working
                            || section.answer.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                if block.stop {
                    Button(AgentsCopy.stopRun) { Task { await section.actions.stop(run) } }
                        .buttonStyle(.carryOnQuiet).disabled(!online || section.actions.phase == .working)
                }
                if block.markDone {
                    Button(AgentsCopy.markDone) { Task { await section.actions.markStepDone(taskServerID: section.taskServerID, projectID: section.projectID, nextStep: section.nextStep) } }
                        .buttonStyle(.carryOnPrimary).disabled(section.actions.phase == .working)
                }
                if block.handOffAgain, let onHandOff {
                    Button(AgentsCopy.handOffAgain, action: onHandOff).buttonStyle(.carryOnQuiet)
                }
                Spacer(minLength: 0)
            }
        }
    }

    @ViewBuilder
    private var earlierRuns: some View {
        if section.runs.count > 1 {
            MacInspectorSectionLabel(AgentsCopy.earlierRuns)
            ForEach(Array(section.runs.dropFirst()), id: \.id) { run in
                Button { section.choose(run) } label: {
                    Text(verbatim: [run.agentName, AgentRunBlock(run, now: section.now(), relay: false).label, run.createdAt.formatted(.dateTime.day().month(.abbreviated))].joined(separator: " · ")).font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.plain).padding(.bottom, 4)
                .accessibilityAddTraits(section.shown?.id == run.id ? .isSelected : [])
            }
        }
    }
}
