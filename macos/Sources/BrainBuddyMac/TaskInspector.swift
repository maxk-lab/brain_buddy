import BrainBuddyAgents
import BrainBuddyCore
import BrainBuddyDesign
import BrainBuddyMacCore
import BrainBuddyWorkspace
import SwiftUI

// The task inspector beside the list (boards "13 Next action", "15 Not now", "16 What is it?",
// "18 Waiting for", "19 Someday", "20 Done"): the Carry On inspector parts over one task, edited
// as a `TaskEditDraft` the window saves as you type (`TaskAutosave`). Every action that changes a
// task's list goes back to the window, which runs it through `requestNavigation`, so pending field
// edits save first.

/// What the inspector asks of the window.
enum TaskInspectorCommand {
    case complete
    case cancel
    case reopen
    /// A board action that moves the task (Not now, Start it, Waiting on someone, …).
    case move(TaskList)
    /// The list picker: the pending fields and the move save together.
    case changeList(TaskList)
    /// Inbox "Bigger than one step": the Inbox clarification's project path for this item.
    case clarifyAsProject
    /// Inbox "Skip": the next Inbox item.
    case select(TaskID)
    /// ⌘S, or Return in the title: save the pending fields now.
    case save
    /// Esc.
    case close
}

enum TaskInspectorRequest: Equatable {
    case pickDay, changeStep
}

/// A project's dot on the inspector: its own colour, else the palette's neutral dot.
func inspectorProjectDot(_ hex: String?) -> BrainBuddyDesign.RGBColor {
    guard let hex, hex.hasPrefix("#"), hex.count == 7, let value = UInt32(hex.dropFirst(), radix: 16) else {
        return CarryOnProjectColor.dust
    }
    return BrainBuddyDesign.RGBColor(value)
}

/// A text field or editor on the inspector card: 14 pt on `field-bg`, radius 8.
private struct InspectorFieldStyle: ViewModifier {
    func body(content: Content) -> some View {
        content
            .textFieldStyle(.plain)
            .font(MacRowFont.title)
            .foregroundStyle(CarryOnColorStyle.coTextPrimary)
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(CarryOnColorStyle.coFieldBg, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }
}

extension View {
    fileprivate func inspectorField() -> some View { modifier(InspectorFieldStyle()) }
}

struct TaskInspector: View {
    /// The inspector column: the boards' 340, and 380 for an Inbox item.
    static func width(for state: TaskState) -> CGFloat { state == .inbox ? 380 : 340 }

    let task: TaskRecord
    let model: BrainBuddyModel
    @Binding var title: String
    @Binding var details: String
    @Binding var waitingFor: String
    @Binding var projectID: ProjectID?
    @Binding var tagIDs: [TagID]
    @Binding var dueDate: CalendarDay?
    /// Why the fields cannot save yet; nil when they can (or nothing is pending).
    let problem: String?
    let editorDirty: Bool
    /// "Inbox · 1 of 4", when the item is among the Inbox items on screen.
    let inboxPosition: (position: Int, total: Int)?
    /// The Inbox item after this one on screen, for "Skip".
    let nextInboxID: TaskID?
    let onCommand: (TaskInspectorCommand) -> Void
    let onFocusChange: (TaskInspectorField?) -> Void
    let onExtrasDirtyChange: (Bool) -> Void
    /// The complete dedicated agent inspector (030 G-05); nil for an ordinary task.
    var runSection: AnyView?
    /// Hand off… (030 G-04): hidden with the relay off, disabled with its reason for an unsynced step.
    var handOff = HandOffControl.hidden
    var onHandOff: () -> Void = {}
    var requestedAction: Binding<TaskInspectorRequest?> = .constant(nil)

    @FocusState private var fieldFocus: TaskInspectorField?
    @FocusState private var dueDayFocused: Bool
    @State private var showingNotNow = false
    @State private var showingDueDay = false
    @State private var restoreDueDayAfterDismissal = false
    @State private var showingExtras = false
    @State private var suggesting: SuggestionRequest?
    /// Inbox "A step I can take": the title is being rewritten as the first step.
    @State private var writingFirstStep = false
    @State private var showingProjectCreator = false
    @State private var newProjectName = ""
    @State private var newProjectEditorID = UUID().uuidString
    @State private var isCreatingProject = false

    private var variant: TaskInspectorVariant { TaskInspectorVariant(state: task.state) }

    @ViewBuilder
    var body: some View {
        if let runSection {
            runSection
        } else {
            ordinaryInspector
        }
    }

    private var ordinaryInspector: some View {
        MacInspectorPanel(width: Self.width(for: task.state)) {
            ScrollView {
                VStack(alignment: .leading, spacing: 0) { content }
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollIndicators(.automatic)
        } actionBar: {
            actionBar
                // presentation-region: task inspector popover and sheets
                .popover(isPresented: $showingNotNow, arrowEdge: .top) {
                    MacNotNowPopoverContent(
                        onPickDay: pickADay,
                        onWaiting: {
                            showingNotNow = false
                            onCommand(.move(.waiting))
                        },
                        onSomeday: {
                            showingNotNow = false
                            onCommand(.move(.someday))
                        },
                        onChangeStep: changeTheStep)
                        .background {
                            Button("Dismiss Not now") { showingNotNow = false }
                                .keyboardShortcut(.cancelAction)
                                .frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
                        }
                }
                // presentation-region-end
        }
        .background { shortcuts }
        // presentation-region: task inspector popover and sheets
        .sheet(isPresented: $showingProjectCreator, onDismiss: {
            newProjectEditorID = UUID().uuidString
            isCreatingProject = false
        }) {
            projectCreator.interactiveDismissDisabled(isCreatingProject)
        }
        .sheet(item: $suggesting) { request in
            ReviewSuggestionSheetLoader(model: model, task: task, kind: request.kind) { suggesting = nil }
        }
        // presentation-region-end
        .onChange(of: requestedAction.wrappedValue, initial: true) { _, request in
            guard let request else { return }
            switch request {
            case .pickDay: pickADay()
            case .changeStep: changeTheStep()
            }
            requestedAction.wrappedValue = nil
        }
        .onChange(of: fieldFocus) { _, field in onFocusChange(field) }
    }

    // MARK: Per list

    @ViewBuilder
    private var content: some View {
        switch variant {
        case .inbox: inboxContent
        case .next: nextContent
        case .waiting: waitingContent
        case .someday: somedayContent
        case .finished(let cancelled): finishedContent(cancelled: cancelled)
        }
    }

    private var showsExtras: Bool { showingExtras || !task.subtasks.isEmpty || !task.comments.isEmpty }

    private var projectName: String? { projectID.map { model.projectLabel($0) } }
    private var projectDot: BrainBuddyDesign.RGBColor? {
        projectID.map { inspectorProjectDot(model.project($0)?.color) }
    }

    private func eyebrow(_ listName: String) -> MacInspectorEyebrow {
        MacInspectorEyebrow(projectName: projectName, projectColor: projectDot, listName: listName)
    }

    /// Board 16: the item as typed, then "What is it?" and its answers.
    @ViewBuilder
    private var inboxContent: some View {
        if let inboxPosition {
            MacInspectorEyebrow.inbox(position: inboxPosition.position, of: inboxPosition.total)
        } else {
            MacInspectorEyebrow(projectName: nil, projectColor: nil, listName: TaskList.inbox.title)
        }
        titleField(item: true)
        problemLine
        if writingFirstStep {
            MacInspectorBodyText(
                text: "Write the first step above. It goes to Next actions.", quiet: true, topPadding: 14)
        } else {
            MacInspectorTitle(text: "What is it?", serif: true, size: 24)
                .padding(.top, 14)
                .padding(.bottom, 12)
            MacInspectorChoiceRow(
                title: "A step I can take", detail: "Write the first step, it goes to Next actions",
                glyph: .board(.nextActions), action: writeTheFirstStep)
            MacInspectorChoiceRow(
                title: "Bigger than one step", detail: "Make it a project with a first step", glyph: .folder
            ) {
                onCommand(.clarifyAsProject)
            }
            .disabled(task.projectID != nil)
            MacInspectorChoiceRow(
                title: "Waiting on someone", detail: "Say who, it goes to Waiting for", glyph: .board(.waitingFor)
            ) {
                onCommand(.move(.waiting))
            }
            MacInspectorChoiceRow(
                title: "Someday", detail: "Not now, keep it for later", glyph: .board(.somedayMaybe)
            ) {
                onCommand(.move(.someday))
            }
        }
        if writingFirstStep { notes(label: "Note") }
        // Keep extras mounted so Back preserves unfinished input and retry IDs. Their TextFields
        // disable correctly; the native Note TextEditor above must be omitted when not authoring.
        TaskDetailExtras(task: task, model: model, onDirtyChange: onExtrasDirtyChange)
            .frame(height: writingFirstStep ? nil : 0, alignment: .top)
            .opacity(writingFirstStep ? 1 : 0)
            .clipped()
            .disabled(!writingFirstStep)
            .allowsHitTesting(writingFirstStep)
            .accessibilityHidden(!writingFirstStep)
        if writingFirstStep { chips }
    }

    /// Board 13.
    @ViewBuilder
    private var nextContent: some View {
        eyebrow(TaskList.next.title)
        titleField()
        problemLine
        chips
        notes(label: "Why")
        TaskDetailExtras(task: task, model: model, onDirtyChange: onExtrasDirtyChange)
    }

    /// Board 18: who it is with, first.
    @ViewBuilder
    private var waitingContent: some View {
        eyebrow(TaskList.waiting.title)
        titleField()
        HStack(spacing: 10) {
            MacInspectorGlyphView(glyph: .person, size: 22)
                .foregroundStyle(CarryOnColorStyle.coTextPrimary)
            VStack(alignment: .leading, spacing: 0) {
                TextField("Person, event, or condition", text: $waitingFor)
                    .textFieldStyle(.plain)
                    .font(MacRowFont.action(size: 15, weight: 600, relativeTo: .body))
                    .foregroundStyle(CarryOnColorStyle.coTextPrimary)
                    .focused($fieldFocus, equals: .waitingFor)
                    .accessibilityLabel("Waiting for person, event, or condition")
                MacInspectorMetaText(text: waitingSince, topPadding: 0)
            }
        }
        .padding(.top, 14)
        problemLine
        notes(label: "Note")
        chips
        TaskDetailExtras(task: task, model: model, onDirtyChange: onExtrasDirtyChange)
            .frame(height: showsExtras ? nil : 0, alignment: .top)
            .clipped()
            .disabled(!showsExtras)
            .allowsHitTesting(showsExtras)
            .accessibilityHidden(!showsExtras)
    }

    /// Board 19: the serif title, how long it has been here, and how to start it.
    @ViewBuilder
    private var somedayContent: some View {
        eyebrow("Someday")
        titleField(serif: true)
        problemLine
        MacInspectorMetaText(text: "Here since \(Self.monthText(task.createdAt))")
        notes(label: "Note")
        MacInspectorBodyText(
            text: "Ready to start? Move it to Next actions.", quiet: true, topPadding: 18)
        TaskDetailExtras(task: task, model: model, onDirtyChange: onExtrasDirtyChange)
            .frame(height: showsExtras ? nil : 0, alignment: .top)
            .clipped()
            .disabled(!showsExtras)
            .allowsHitTesting(showsExtras)
            .accessibilityHidden(!showsExtras)
    }

    /// Board 20: read-only, with "Reopen…".
    @ViewBuilder
    private func finishedContent(cancelled: Bool) -> some View {
        let date = cancelled ? task.cancelledAt : task.completedAt
        let word = cancelled ? "Cancelled" : "Done"
        MacInspectorEyebrow(
            projectName: task.projectID.map { model.projectLabel($0) },
            projectColor: task.projectID.map { inspectorProjectDot(model.project($0)?.color) },
            listName: date.map { "\(word) on \($0.formatted(date: .abbreviated, time: .omitted))" } ?? word)
        MacInspectorTitle(text: task.title, struck: true)
        if let note = task.details, !note.isEmpty {
            MacInspectorSectionLabel("Note")
            MacInspectorBodyText(text: note)
        }
        if !task.subtasks.isEmpty {
            MacInspectorSectionLabel("Steps inside")
            VStack(alignment: .leading, spacing: 8) {
                ForEach(task.subtasks.sorted { $0.orderKey < $1.orderKey }) { subtask in
                    MacInspectorStepLine(title: subtask.title, done: subtask.state == .completed)
                }
            }
        }
        if !task.comments.isEmpty {
            MacInspectorSectionLabel("Comments")
            VStack(alignment: .leading, spacing: 8) {
                ForEach(task.comments) { comment in
                    MacInspectorBodyText(text: comment.body)
                }
            }
        }
        MacInspectorMetaText(text: "Finished steps are read-only.", topPadding: 16)
    }

    // MARK: Parts

    private func titleField(serif: Bool = false, item: Bool = false) -> some View {
        MacInspectorTitleField(text: $title, serif: serif, item: item)
            .focused($fieldFocus, equals: .title)
            .onSubmit {
                if writingFirstStep, variant == .inbox {
                    onCommand(.move(.next))
                } else {
                    onCommand(.save)
                }
            }
    }

    @ViewBuilder
    private var problemLine: some View {
        if let problem {
            Text(problem)
                .font(MacRowFont.meta)
                .foregroundStyle(CarryOnColorStyle.coStatusDanger)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 6)
        }
    }

    private var projectChoices: some View {
        Group {
            Button("No project") { projectID = nil }
            ForEach(model.projects) { project in
                Button(project.name) { projectID = project.id }
            }
            Button("New project…") {
                newProjectEditorID = UUID().uuidString
                showingProjectCreator = true
            }
        }
    }

    private var chips: some View {
        MacInspectorFlow(spacing: 6) {
            if variant != .waiting || projectID != nil {
                Menu { projectChoices } label: {
                    CarryOnChip(projectName ?? "+ Project", dot: projectDot)
                }
                .accessibilityLabel("Project")
            }
            // Retain the trigger in the hierarchy so More can open the same date popover.
            dueDayChip
                .frame(width: variant == .waiting && dueDate == nil ? 0 : nil)
                .clipped()
                .allowsHitTesting(variant != .waiting || dueDate != nil)
                .accessibilityHidden(variant == .waiting && dueDate == nil)
            ForEach(tagIDs, id: \.self) { id in
                if let tag = model.tag(id) {
                    Menu {
                        Button("Remove tag") { tagIDs.removeAll { $0 == id } }
                    } label: { CarryOnChip(tag.name) }
                }
            }
            Menu {
                if variant == .waiting {
                    if projectID == nil { Menu("Project") { projectChoices } }
                    if dueDate == nil { Button("Due date…") { showingDueDay = true } }
                }
                Picker("List", selection: Binding(
                    get: { task.state.openList ?? .next }, set: { onCommand(.changeList($0)) })
                ) {
                    ForEach(TaskList.allCases) { Text($0.title).tag($0) }
                }
                Menu("Add tag") {
                    ForEach(model.tags.filter { !tagIDs.contains($0.id) }) { tag in
                        Button(tag.name) { tagIDs.append(tag.id) }
                    }
                }
                Button("Steps and comments…") { showingExtras = true }
                if task.state == .waiting {
                    Button("Move to Next") { onCommand(.move(.next)) }
                }
                suggestionEntry
                if variant == .next || variant == .waiting { handOffMenu }
                Button("Cancel task", role: .destructive) { onCommand(.cancel) }
            } label: { CarryOnChip("…") }
            .accessibilityLabel("More task properties")
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .menuIndicator(.hidden)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.top, 14)
    }

    private var dueDayChip: some View {
        Button { showingDueDay = true } label: {
            CarryOnChip(dueDate.map(Self.dueText) ?? "+ Date", tone: dueDate.map(Self.dueTone) ?? .plain)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Due date")
        .focusable(variant != .waiting || dueDate != nil)
        .focused($dueDayFocused)
        // presentation-region: task inspector popover and sheets
        .popover(isPresented: $showingDueDay, arrowEdge: .bottom) {
            InspectorDueDayPicker(dueDate: dueDate, onCancel: closeDueDayPicker) { selected in
                dueDate = selected
                closeDueDayPicker()
            }
            .onDisappear(perform: restoreDueDayTrigger)
        }
        // presentation-region-end
    }

    private func notes(label: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            MacInspectorSectionLabel(label)
            TextField("Add a note…", text: $details, axis: .vertical)
                .textFieldStyle(.plain)
                .font(MacRowFont.title)
                .lineSpacing(5)
                .foregroundStyle(CarryOnColorStyle.coTextPrimary)
                .focused($fieldFocus, equals: .details)
                .accessibilityLabel(label == "Why" ? "Why, notes and context" : "Note")
        }
    }

    /// The bounded entry of the Weekly Review suggestions (D-04): only on a Next action while the
    /// review is exposed. A choice here opens the sheet; nothing is requested or changed by it.
    @ViewBuilder
    private var suggestionEntry: some View {
        switch model.reviewSuggestionEntry(for: task, editorDirty: editorDirty) {
        case .hidden:
            EmptyView()
        case .blocked(let message):
            Text("Suggest with AI… — \(message)")
        case .available(let kinds):
            Menu("Suggest with AI…") {
                ForEach(kinds, id: \.rawValue) { kind in
                    Button(kind == .firstStep ? "Suggest a first step" : "Suggest clearer wording") {
                        suggesting = SuggestionRequest(kind: kind)
                    }
                }
            }
            .accessibilityLabel("Suggest with AI for \(task.title)")
        }
    }

    @ViewBuilder
    private var handOffMenu: some View {
        switch handOff {
        case .hidden:
            EmptyView()
        case .enabled:
            Button(AgentsCopy.handOff, action: onHandOff)
        case .disabled(let reason):
            Button("\(AgentsCopy.handOff) — \(reason)", action: onHandOff).disabled(true)
        }
    }

    // MARK: Action bar

    @ViewBuilder
    private var actionBar: some View {
        switch variant {
        case .inbox:
            if writingFirstStep {
                MacInspectorActionBar(actions: [
                    .primary("Make it the next step") { onCommand(.move(.next)) },
                    .quiet("Back", perform: returnFromFirstStep),
                ])
            } else {
                MacInspectorActionBar(
                    actions: [.secondary("Done already") { onCommand(.complete) }, .quiet("Not needed") { onCommand(.cancel) }],
                    trailingNote: nextInboxID == nil ? nil : "Skip",
                    onTrailingNote: skipAction)
            }
        case .next:
            MacInspectorActionBar(
                actions: [.primary("Done") { onCommand(.complete) }, .secondary("Not now") { showingNotNow = true }],
                trailingNote: problem == nil ? "Saved as you type" : "Not saved yet")
        case .waiting:
            HStack(spacing: 8) {
                Button("Got it") { onCommand(.complete) }.buttonStyle(.carryOnPrimary)
                Button("Follow up") {}.buttonStyle(.carryOnQuiet).disabled(true)
                    .help("Separate follow-up creation is unavailable in this inspector.")
                Spacer(minLength: 0)
            }
        case .someday:
            MacInspectorActionBar(actions: [
                .primary("Start it") { onCommand(.move(.next)) },
                .quiet("Not needed") { onCommand(.cancel) },
            ])
        case .finished:
            MacInspectorActionBar(actions: [.secondary("Reopen…") { onCommand(.reopen) }])
        }
    }

    /// "Skip", only while there is a next Inbox item.
    private var skipAction: (() -> Void)? {
        guard let next = nextInboxID else { return nil }
        return { onCommand(.select(next)) }
    }

    /// ⌘S saves now; Esc closes. Invisible, so only their shortcuts show.
    private var shortcuts: some View {
        ZStack {
            Button("Save task now") { onCommand(.save) }
                .keyboardShortcut("s", modifiers: [.command])
            Button("Close task details") { onCommand(.close) }
                .keyboardShortcut(.cancelAction)
                .disabled(showingDueDay || showingNotNow)
        }
        .frame(width: 0, height: 0)
        .opacity(0)
        .accessibilityHidden(true)
    }

    // MARK: Not now and the first step

    private func pickADay() {
        showingNotNow = false
        Task { @MainActor in showingDueDay = true }
    }

    // presentation-region: task inspector focus
    private func closeDueDayPicker() {
        dueDayFocused = false
        restoreDueDayAfterDismissal = true
        showingDueDay = false
    }

    private func restoreDueDayTrigger() {
        guard restoreDueDayAfterDismissal else { return }
        restoreDueDayAfterDismissal = false
        Task { @MainActor in dueDayFocused = true }
    }

    private func changeTheStep() {
        showingNotNow = false
        Task { @MainActor in fieldFocus = .title }
    }

    private func writeTheFirstStep() {
        writingFirstStep = true
        fieldFocus = .title
    }

    private func returnFromFirstStep() {
        writingFirstStep = false
        fieldFocus = .title
    }

    // presentation-region-end

    // MARK: Project creator

    private var projectCreator: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("New project").font(MacRowFont.titleStrong)
            TextField("Project name", text: $newProjectName)
                .inspectorField()
                .disabled(isCreatingProject)
            HStack {
                Spacer()
                Button("Cancel") {
                    showingProjectCreator = false
                    newProjectEditorID = UUID().uuidString
                }
                .buttonStyle(.carryOnQuiet)
                .keyboardShortcut(.cancelAction)
                .disabled(isCreatingProject)
                Button("Add project") {
                    guard !isCreatingProject else { return }
                    let authored = newProjectName
                    let editorID = newProjectEditorID
                    isCreatingProject = true
                    Task {
                        let created = await model.createProject(authored, editorID: editorID)
                        guard newProjectEditorID == editorID, isCreatingProject else { return }
                        isCreatingProject = false
                        guard showingProjectCreator, newProjectName == authored, let created else { return }
                        projectID = created
                        newProjectName = ""
                        showingProjectCreator = false
                        newProjectEditorID = UUID().uuidString
                    }
                }
                .buttonStyle(.carryOnPrimary)
                .keyboardShortcut(.defaultAction)
                .disabled(isCreatingProject || newProjectName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(22)
        .frame(width: 340)
        .background(CarryOnColorStyle.coSurfaceCard)
    }

    // MARK: Words

    private func tagNames(_ ids: [TagID]) -> [String] {
        ids.compactMap { model.tag($0).map { "#\($0.name)" } }
    }

    private var waitingSince: String {
        guard let since = task.waitingSince else { return "Waiting for an answer" }
        return "Asked \(since.formatted(.relative(presentation: .named)))"
    }

    static func dueText(_ due: CalendarDay) -> String {
        "Due \(due.startDate().formatted(.dateTime.month(.abbreviated).day()))"
    }

    static func dueTone(_ due: CalendarDay) -> CarryOnChip.Tone {
        switch MacDueTone(due: due, today: CalendarDay(date: Date())) {
        case .plain: .plain
        case .due: .dueSoon
        case .overdue: .overdue
        }
    }

    static func monthText(_ date: Date) -> String {
        let sameYear = Calendar.current.isDate(date, equalTo: Date(), toGranularity: .year)
        return sameYear ? date.formatted(.dateTime.month(.wide)) : date.formatted(.dateTime.month(.wide).year())
    }
}

// MARK: - Steps inside, comments and suggestions

private struct SuggestionRequest: Identifiable {
    let kind: SuggestionKind
    var id: String { kind.rawValue }
}

/// "Steps inside" and "Comments": each saves at once through the kit, as before; then the
/// Weekly Review suggestion entry.
struct TaskDetailExtras: View {
    let task: TaskRecord
    let model: BrainBuddyModel
    let onDirtyChange: (Bool) -> Void
    @State private var newSubtask = ""
    @State private var newComment = ""
    @State private var newSubtaskEditorID = UUID().uuidString
    @State private var newCommentEditorID = UUID().uuidString
    @State private var modifiedIDs: Set<String> = []
    @State private var subtaskTitles: [SubtaskID: String] = [:]
    @State private var commentBodies: [CommentID: String] = [:]
    @State private var editingCommentIDs: Set<CommentID> = []
    @State private var addingStep = false
    @State private var addingComment = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            sectionHeading("Steps inside", adding: $addingStep)
            VStack(alignment: .leading, spacing: 8) {
                ForEach(task.subtasks.sorted { $0.orderKey < $1.orderKey }) { subtask in
                    SubtaskLine(
                        taskID: task.id, subtask: subtask, model: model, title: subtaskTitle(for: subtask),
                        onSaved: {
                            subtaskTitles.removeValue(forKey: subtask.id)
                            setModified("subtask-\(subtask.id.rawValue)", dirty: false)
                        }
                    )
                }
                HStack(spacing: 8) {
                    TextField("New step", text: $newSubtask)
                        .inspectorField()
                        .onSubmit { addSubtask() }
                    Button("Add") { addSubtask() }
                        .buttonStyle(.carryOnSecondary)
                        .accessibilityLabel("Add subtask")
                        .disabled(
                            newSubtask.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                || !EditorLimits.fits(newSubtask, EditorLimits.title)
                        )
                }
                .frame(height: addingStep ? nil : 0)
                .clipped()
                .disabled(!addingStep)
                .allowsHitTesting(addingStep)
                .accessibilityHidden(!addingStep)
            }
            sectionHeading("Comments", adding: $addingComment)
            VStack(alignment: .leading, spacing: 8) {
                if task.comments.isEmpty {
                    MacInspectorBodyText(text: "None yet", quiet: true)
                }
                ForEach(task.comments) { comment in
                    CommentLine(
                        taskID: task.id, comment: comment, model: model, canEdit: model.isOwnComment(comment),
                        editing: commentEditing(for: comment), draftBody: commentBody(for: comment),
                        onFinished: {
                            commentBodies.removeValue(forKey: comment.id)
                            setModified("comment-\(comment.id.rawValue)", dirty: false)
                        }
                    )
                }
                VStack(alignment: .leading, spacing: 8) {
                TextField("Write a comment", text: $newComment, axis: .vertical)
                    .lineLimit(2...4)
                    .inspectorField()
                Button("Add comment") {
                    let authored = newComment
                    let editorID = newCommentEditorID
                    Task {
                        if await model.addComment(to: task.id, body: authored, editorID: editorID) {
                            newComment = ""
                            newCommentEditorID = UUID().uuidString
                        }
                    }
                }
                .buttonStyle(.carryOnSecondary)
                .disabled(
                    newComment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                        || !EditorLimits.fits(newComment, EditorLimits.comment)
                )
                }
                .frame(height: addingComment ? nil : 0)
                .clipped()
                .disabled(!addingComment)
                .allowsHitTesting(addingComment)
                .accessibilityHidden(!addingComment)
            }
        }
        .onChange(of: newSubtask) { _, _ in reportDirty() }
        .onChange(of: newComment) { _, _ in reportDirty() }
    }

    private func sectionHeading(_ title: String, adding: Binding<Bool>) -> some View {
        HStack(alignment: .firstTextBaseline) {
            MacInspectorSectionLabel(title)
            Button { adding.wrappedValue.toggle() } label: { Text("+") }
                .buttonStyle(.carryOnQuiet)
                .accessibilityLabel(title == "Steps inside" ? "Add subtask" : "Write a comment")
        }
    }

    private func setModified(_ id: String, dirty: Bool) {
        if dirty { modifiedIDs.insert(id) } else { modifiedIDs.remove(id) }
        reportDirty()
    }

    private func subtaskTitle(for subtask: SubtaskRecord) -> Binding<String> {
        Binding(
            get: { subtaskTitles[subtask.id] ?? subtask.title },
            set: { value in
                subtaskTitles[subtask.id] = value
                setModified("subtask-\(subtask.id.rawValue)", dirty: value != subtask.title)
            }
        )
    }

    private func commentEditing(for comment: CommentRecord) -> Binding<Bool> {
        Binding(
            get: { editingCommentIDs.contains(comment.id) },
            set: { editing in
                if editing { editingCommentIDs.insert(comment.id) } else { editingCommentIDs.remove(comment.id) }
                setModified(
                    "comment-\(comment.id.rawValue)", dirty: editing && (commentBodies[comment.id] ?? comment.body) != comment.body
                )
            }
        )
    }

    private func commentBody(for comment: CommentRecord) -> Binding<String> {
        Binding(
            get: { commentBodies[comment.id] ?? comment.body },
            set: { value in
                commentBodies[comment.id] = value
                setModified(
                    "comment-\(comment.id.rawValue)", dirty: editingCommentIDs.contains(comment.id) && value != comment.body
                )
            }
        )
    }

    private func reportDirty() {
        onDirtyChange(
            !newSubtask.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                || !newComment.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !modifiedIDs.isEmpty
        )
    }

    private func addSubtask() {
        let authored = newSubtask
        let editorID = newSubtaskEditorID
        Task {
            if await model.addSubtask(to: task.id, title: authored, editorID: editorID) {
                newSubtask = ""
                newSubtaskEditorID = UUID().uuidString
            }
        }
    }
}

private struct SubtaskLine: View {
    let taskID: TaskID
    let subtask: SubtaskRecord
    let model: BrainBuddyModel
    @Binding var title: String
    let onSaved: () -> Void
    @State private var editorID = UUID().uuidString

    var body: some View {
        HStack(spacing: 10) {
            Button {
                Task {
                    if await model.toggleSubtask(subtask, in: taskID, editorID: editorID) { editorID = UUID().uuidString }
                }
            } label: {
                MacInspectorStepMark(done: subtask.state == .completed)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(subtask.state == .completed ? "Reopen" : "Complete") \(subtask.title)")
            TextField("Subtask title", text: $title)
                .textFieldStyle(.plain)
                .font(MacRowFont.title)
                .foregroundStyle(
                    subtask.state == .completed ? CarryOnColorStyle.coStepTextDone : CarryOnColorStyle.coTextPrimary)
                .onSubmit { saveTitle() }
            if title != subtask.title {
                Button("Save") { saveTitle() }
                    .buttonStyle(.carryOnQuiet)
                    .disabled(
                        title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            || !EditorLimits.fits(title, EditorLimits.title)
                    )
            }
        }
    }

    private func saveTitle() {
        guard title != subtask.title else { return }
        let authored = title
        Task {
            if await model.renameSubtask(subtask.id, in: taskID, title: authored, editorID: editorID) {
                editorID = UUID().uuidString
                onSaved()
            }
        }
    }
}

private struct CommentLine: View {
    let taskID: TaskID
    let comment: CommentRecord
    let model: BrainBuddyModel
    let canEdit: Bool
    @Binding var editing: Bool
    @Binding var draftBody: String
    let onFinished: () -> Void
    @State private var editorID = UUID().uuidString

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            if editing {
                TextField("Comment", text: $draftBody, axis: .vertical)
                    .lineLimit(2...4)
                    .inspectorField()
                HStack {
                    Button("Save") {
                        let authored = draftBody
                        Task {
                            if await model.editComment(comment.id, in: taskID, body: authored, editorID: editorID) {
                                editing = false
                                editorID = UUID().uuidString
                                onFinished()
                            }
                        }
                    }
                    .buttonStyle(.carryOnSecondary)
                    .disabled(
                        draftBody.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                            || !EditorLimits.fits(draftBody, EditorLimits.comment)
                    )
                    Button("Cancel") {
                        editing = false
                        editorID = UUID().uuidString
                        onFinished()
                    }
                    .buttonStyle(.carryOnQuiet)
                }
            } else {
                MacInspectorBodyText(text: comment.body)
                if canEdit {
                    Button("Edit comment") { editing = true }
                        .buttonStyle(.plain)
                        .font(MacRowFont.meta)
                        .foregroundStyle(CarryOnColorStyle.coTextSecondary)
                }
            }
        }
        .padding(8)
        .background(CarryOnColorStyle.coBgGround, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
    }
}
