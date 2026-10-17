import BrainBuddyCore
import BrainBuddyDesign
import SwiftUI

/// Board M14: local Gregorian days only, applied through the inspector's existing draft.
struct InspectorDueDayPicker: View {
    let onPick: (CalendarDay?) -> Void
    private let onCancel: (() -> Void)?
    @Environment(\.dismiss) private var dismiss
    private enum Control: Hashable {
        case quick(String), day(CalendarDay), previousMonth, nextMonth, clear, confirm
    }
    @FocusState private var focus: Control?
    private let today: CalendarDay
    @State private var selected: CalendarDay
    @State private var month: CalendarDay

    init(
        dueDate: CalendarDay?, today: CalendarDay = CalendarDay(date: Date()),
        onCancel: (() -> Void)? = nil, onPick: @escaping (CalendarDay?) -> Void
    ) {
        self.onPick = onPick
        self.onCancel = onCancel
        self.today = today
        let day = dueDate ?? today
        _selected = State(initialValue: day)
        _month = State(initialValue: CalendarDay(year: day.year, month: day.month, day: 1)!)
    }

    private var calendar: Calendar { Calendar(identifier: .gregorian) }
    private var monthDays: [CalendarDay?] {
        let weekday = calendar.component(.weekday, from: month.startDate(in: calendar))
        let leading = (weekday + 5) % 7 // Monday first, as on the board.
        let count = calendar.range(of: .day, in: .month, for: month.startDate(in: calendar))!.count
        return Array(repeating: nil, count: leading) + (0..<count).map { month.adding(days: $0) }
    }

    static func quickDays(today: CalendarDay) -> [(String, CalendarDay)] {
        let weekday = Calendar(identifier: .gregorian).component(.weekday, from: today.startDate())
        let saturday = (7 - weekday + 7) % 7
        let monday = (9 - weekday) % 7
        return [("Today", today), ("Tomorrow", today.adding(days: 1)),
                ("Saturday", today.adding(days: saturday == 0 ? 7 : saturday)),
                ("Next week", today.adding(days: monday == 0 ? 7 : monday))]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Due date").font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
            MacInspectorFlow(spacing: 6) {
                ForEach(Self.quickDays(today: today), id: \.0) { title, day in
                    Button {
                        selected = day
                        month = CalendarDay(year: day.year, month: day.month, day: 1)!
                    } label: { CarryOnChip(title, isSelected: selected == day) }
                    .buttonStyle(.plain)
                    .focused($focus, equals: .quick(title))
                }
            }
            HStack {
                Text(month.startDate().formatted(.dateTime.month(.wide).year())).font(MacRowFont.titleStrong)
                Spacer()
                monthButton("Previous month", symbol: "chevron.left", offset: -1)
                monthButton("Next month", symbol: "chevron.right", offset: 1)
            }
            LazyVGrid(columns: Array(repeating: GridItem(.flexible(), spacing: 0), count: 7), spacing: 4) {
                ForEach(Array(["M", "T", "W", "T", "F", "S", "S"].enumerated()), id: \.offset) { _, name in
                    Text(name).font(MacRowFont.meta).foregroundStyle(CarryOnColorStyle.coTextSecondary)
                }
                ForEach(Array(monthDays.enumerated()), id: \.offset) { _, day in
                    if let day {
                        Button { selected = day } label: {
                            Text("\(day.day)")
                                .font(MacRowFont.title)
                                .foregroundStyle(selected == day ? CarryOnColorStyle.coBgGround
                                    : (day < today ? CarryOnColorStyle.coTextSecondary : CarryOnColorStyle.coTextPrimary))
                                .frame(width: 28, height: 28)
                                .background {
                                    if selected == day { Circle().fill(CarryOnColorStyle.coTextPrimary) }
                                }
                                .overlay { if day == today { Circle().strokeBorder(CarryOnColorStyle.coTextPrimary, lineWidth: 1) } }
                        }
                        .buttonStyle(.plain)
                        .focused($focus, equals: .day(day))
                        .accessibilityLabel(day.startDate().formatted(date: .complete, time: .omitted))
                        .accessibilityAddTraits(selected == day ? .isSelected : [])
                    } else { Color.clear.frame(height: 28) }
                }
            }
            Text("A day, not a time. In Today on that day, overdue after.")
                .fixedSize(horizontal: false, vertical: true)
                .font(MacRowFont.meta)
                .foregroundStyle(CarryOnColorStyle.coTextSecondary)
            HStack {
                Button("No date") { onPick(nil) }
                    .buttonStyle(.carryOnQuiet)
                    .focused($focus, equals: .clear)
                Spacer()
                Button("Due \(selected.startDate().formatted(.dateTime.weekday(.wide).day()))") { onPick(selected) }
                    .buttonStyle(.carryOnStrong)
                    .focused($focus, equals: .confirm)
            }
        }
        .defaultFocus($focus, .quick("Today"), priority: .userInitiated)
        .background {
            // One Escape owner inside the popover; cancellation never applies the selected day.
            Button("Dismiss due date") {
                if let onCancel { onCancel() } else { dismiss() }
            }
            .keyboardShortcut(.cancelAction)
            .frame(width: 0, height: 0)
            .opacity(0)
            .accessibilityHidden(true)
        }
        .padding(14)
        .frame(width: 300)
        .background(MacBoardSurface.popover)
    }

    private func monthButton(_ label: String, symbol: String, offset: Int) -> some View {
        Button {
            if let date = calendar.date(byAdding: .month, value: offset, to: month.startDate(in: calendar)) {
                month = CalendarDay(date: date, calendar: calendar)
            }
        } label: { Image(systemName: symbol) }
        .buttonStyle(.carryOnQuiet)
        .focused($focus, equals: offset < 0 ? .previousMonth : .nextMonth)
        .accessibilityLabel(label)
        .disabled(offset < 0 ? month.year == 1 && month.month == 1 : month.year == 9999 && month.month == 12)
    }
}
