#if canImport(SwiftUI) && os(macOS)
import BrainBuddyCore
import Testing
@testable import BrainBuddyMac

@Suite("Inspector day picker")
@MainActor
struct InspectorDueDayTests {
    @Test("M14 quick days stay calendar days across a month and year boundary")
    func quickDays() throws {
        let today = try #require(CalendarDay(year: 2026, month: 12, day: 31))
        let choices = InspectorDueDayPicker.quickDays(today: today)
        #expect(choices.map(\.0) == ["Today", "Tomorrow", "Saturday", "Next week"])
        #expect(choices.map { $0.1.isoString } == ["2026-12-31", "2027-01-01", "2027-01-02", "2027-01-04"])
    }

    @Test("M14 quick days use day arithmetic over the local daylight-saving weekend")
    func daylightSaving() throws {
        let today = try #require(CalendarDay(year: 2026, month: 10, day: 24))
        let choices = InspectorDueDayPicker.quickDays(today: today)
        #expect(choices[1].1.isoString == "2026-10-25")
        #expect(choices[3].1.isoString == "2026-10-26")
    }
}
#endif
