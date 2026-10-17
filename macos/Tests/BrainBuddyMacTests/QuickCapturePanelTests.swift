#if canImport(SwiftUI) && os(macOS)
import AppKit
import BrainBuddyCore
import BrainBuddyDesign
import BrainBuddyPersistence
import BrainBuddyWorkspace
import SwiftUI
import Testing

@testable import BrainBuddyMac
@testable import BrainBuddyMacCore

@Suite("Quick Capture panel", .serialized)
@MainActor
struct QuickCapturePanelTests {
    @Test("Opening Quick Capture takes text input from an existing native window")
    func focusesCaptureField() async throws {
        _ = NSApplication.shared
        let folder = TemporaryFolder()
        let workspace = Workspace(store: InMemoryDocumentStore(), sync: nil)
        await workspace.load()
        let model = BrainBuddyModel(
            workspace: workspace, localStateStore: MacLocalStateStore(directory: folder.url))
        // Reproduce the hot-key host: an existing app window already owns text input.
        let existingField = NSTextField(string: "Existing draft")
        let existingWindow = NSWindow(
            contentRect: NSRect(x: -10_000, y: -10_000, width: 520, height: 160),
            styleMask: [.titled], backing: .buffered, defer: false)
        existingWindow.isReleasedWhenClosed = false
        existingWindow.contentView = existingField
        existingWindow.makeKeyAndOrderFront(nil)
        #expect(existingWindow.makeFirstResponder(existingField))
        #expect(existingField.currentEditor() != nil)
        defer { existingWindow.close() }

        let size = NSSize(width: 520, height: 192)
        let panel = QuickCapturePanel(
            contentRect: NSRect(origin: NSPoint(x: -10_000, y: -10_000), size: size),
            styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false
        )
        panel.isReleasedWhenClosed = false
        panel.contentView = NSHostingView(
            rootView: QuickCaptureView(model: model, shortcut: nil, onClose: {})
                .environment(\.carryOnPlatform, .mac)
        )
        defer { panel.close() }

        panel.makeKeyAndOrderFront(nil)

        // Let AppKit commit the SwiftUI subtree and its default-focus request, as in native-host
        // snapshot tests. No first responder is assigned by the test.
        await withCheckedContinuation { continuation in
            DispatchQueue.main.async { continuation.resume() }
        }
        #expect(panel.isKeyWindow)
        #expect(NSApp.keyWindow === panel, "capture takes keyboard input from the existing window")

        let views = descendants(of: try #require(panel.contentView))
        // SwiftUI exposes accessibilityIdentifier on its accessibility element, not on the
        // backing NSTextField. Identify the one editable field by its production placeholder.
        let field = try #require(views.compactMap { $0 as? NSTextField }.first {
            $0.isEditable && $0.placeholderString == "What's on your mind?"
        }, "the hosted QuickCaptureView exposes its actual title field")
        let editor = try #require(field.currentEditor() as? NSTextView, "the title field entered editing")
        #expect(panel.firstResponder === editor, "initial typing is routed to the capture title")
    }

    private func descendants(of view: NSView) -> [NSView] {
        [view] + view.subviews.flatMap(descendants)
    }
}
#endif
