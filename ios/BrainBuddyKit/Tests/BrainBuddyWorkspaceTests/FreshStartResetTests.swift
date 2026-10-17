import BrainBuddyAPI
import BrainBuddyCore
import BrainBuddyPersistence
import Foundation
import Synchronization
import Testing

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

@testable import BrainBuddyWorkspace

/// The fresh-start reset engine (spec 027, FR-021, design M-02/D-03): what is counted before the
/// person is asked, that the legacy inventory is gone afterwards, the durable `confirmed` / `done`
/// markers around fixed-order idempotent steps, resuming after a kill without asking again, and that
/// nothing may use the new server until `done`. The UI is a later slice; this is the engine only.
@Suite("Fresh-start reset engine (027-FR-021, 027-FR-004)")
struct FreshStartResetTests {
    // MARK: Counts and the decision to ask

    @Test("027-FR-021: tasks, waiting changes and drafts are counted before anything is deleted")
    func countsBeforeAnythingIsDeleted() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        let store = FileDocumentStore(fileURL: lane.documentURL)
        _ = try await store.update { document in
            document.base = Fixture.base([Fixture.serverTask("t1", "One"), Fixture.serverTask("t2", "Two")])
            document.outbox = [PendingOperation(command: .deleteTag("gone"), issuedAt: Fixture.epoch)]
            document.local.formDrafts[DraftKey(rawValue: "form:a")] = FormDraft(text: "typed", savedAt: Fixture.epoch)
        }
        try lane.write(Data("{}".utf8), lane.layout.supportFolder, "ConflictDrafts", "6162.json")
        let reset = lane.reset()

        let counts = await reset.counts()
        #expect(counts == FreshStartCounts(tasks: 2, waitingChanges: 1, drafts: 2))
        #expect(await reset.needsConfirmation())
        #expect(try await reset.phase() == .notStarted)
        #expect(FileManager.default.fileExists(atPath: lane.documentURL.path), "counting deletes nothing")
    }

    @Test("027-FR-021: a store that cannot be read has unknown counts and still asks")
    func unreadableStoreAsks() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        try lane.write(Fixture.garbage, lane.layout.documentFolder, "store.json")
        let reset = lane.reset()
        #expect(await reset.counts() == FreshStartCounts(tasks: nil, waitingChanges: nil, drafts: nil))
        #expect(await reset.needsConfirmation())
    }

    @Test("027-FR-021: leftovers with nothing the person wrote are removed without asking")
    func nothingToLoseIsRemovedWithoutAsking() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        _ = try await FileDocumentStore(fileURL: lane.documentURL).update { _ in }
        try lane.tokens.setToken("old", for: Fixture.serverURL)
        let reset = lane.reset()

        #expect(await reset.counts() == FreshStartCounts(tasks: 0, waitingChanges: 0, drafts: 0))
        #expect(await reset.needsConfirmation() == false)
        try await reset.proceedWithoutAsking()
        #expect(try await reset.phase() == .done)
        #expect(!FileManager.default.fileExists(atPath: lane.documentURL.path))
        #expect(try lane.tokens.token(for: Fixture.serverURL) == nil)
    }

    @Test("027-FR-021: a fresh install with no earlier store has nothing to ask about")
    func freshInstall() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        let reset = lane.reset()
        #expect(await reset.needsConfirmation() == false)
        try await reset.proceedWithoutAsking()
        #expect(try await reset.phase() == .done)
    }

    @Test("027-FR-021: earlier content is never deleted without the person's confirmation")
    func contentNeedsConfirmation() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        _ = try await FileDocumentStore(fileURL: lane.documentURL).update {
            $0.base = Fixture.base([Fixture.serverTask("t1", "One")])
        }
        let reset = lane.reset()
        await #expect(throws: FreshStartResetError.confirmationRequired) { try await reset.proceedWithoutAsking() }
        #expect(FileManager.default.fileExists(atPath: lane.documentURL.path))
        #expect(try await reset.phase() == .notStarted)
        #expect(!FileManager.default.fileExists(atPath: lane.markerURL.path), "Not now writes nothing")
        // A launch that was not confirmed has nothing to resume.
        #expect(try await reset.resume() == false)
    }

    // MARK: The inventory is gone

    @Test("027-FR-021: every listed legacy artifact is gone after the reset, and the inventory has no duplicate")
    func everyListedArtifactIsGone() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        #expect(Set(LegacyArtifacts.all.map(\.id)).count == LegacyArtifacts.all.count)
        for artifact in LegacyArtifacts.all {
            try lane.plant(artifact)
            #expect(try lane.isPresent(artifact), "\(artifact.id) was planted")
        }
        let reset = lane.reset()
        try await reset.confirm()
        for artifact in LegacyArtifacts.all {
            #expect(try !lane.isPresent(artifact), "\(artifact.id) must be gone")
        }
        #expect(try await reset.phase() == .done)
        #expect(lane.httpCleared.withLock { $0 })
        #expect(lane.finished.withLock { $0 } == 1)
    }

    @Test("027-FR-021: the inventory names the artifacts the spec lists")
    func inventoryCoversTheSpecList() {
        let ids = Set(LegacyArtifacts.all.map(\.id))
        for required in [
            "store.document", "store.lock", "store.temporary", "store.quarantine", "store.backups",
            "drafts.conflict.document", "drafts.conflict.support", "drafts.reviewSuggestions", "runtime",
            "defaults.pilotSwitch", "defaults.recentSearches", "defaults.listOptions", "keychain.session",
            "keychain.pendingLogouts", "http.cachesAndCookies",
        ] {
            #expect(ids.contains(required), "\(required) is missing from the inventory")
        }
        #expect(
            LegacyArtifacts.all.contains { $0.location == .defaultsKey(RustRuntimePilotSwitch.defaultsKey) },
            "the pilot switch key is the one the switch uses")
        #expect(LegacyArtifacts.all.contains { $0.location == .runtimeFolder })
    }

    @Test("027-FR-021: data the reset does not own is left alone")
    func unrelatedDataSurvives() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        try lane.write(Data("m".utf8), lane.layout.documentFolder, "mac-local.json")
        try lane.write(Data("w".utf8), lane.layout.documentFolder, "Whisper", "model.bin")
        UserDefaults(suiteName: lane.suite)?.set("kept", forKey: "BBFirstRunRecord")
        let reset = lane.reset()
        try await reset.proceedWithoutAsking()
        #expect(FileManager.default.fileExists(atPath: lane.documentFolder.appendingPathComponent("mac-local.json").path))
        #expect(FileManager.default.fileExists(atPath: lane.documentFolder.appendingPathComponent("Whisper/model.bin").path))
        #expect(UserDefaults(suiteName: lane.suite)?.string(forKey: "BBFirstRunRecord") == "kept")
    }

    // MARK: Markers, order and idempotence

    @Test("027-FR-021: reset=confirmed is written first, the steps follow in the inventory's order, reset=done last")
    func markersAndOrder() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        for artifact in LegacyArtifacts.all { try lane.plant(artifact) }
        let files = RecordingFiles()
        let reset = lane.reset(files: files)
        try await reset.confirm()

        let log = files.log.withLock { $0 }
        #expect(log.first == "write:fresh-start:reset=confirmed")
        #expect(log.last == "write:fresh-start:reset=done")
        let removed = log.filter { $0.hasPrefix("remove:") }.map { String($0.dropFirst("remove:".count)) }
        func index(_ name: String) throws -> Int { try #require(removed.firstIndex(of: name), "\(name) was removed") }
        #expect(try index("runtime") < index("store.json"), "the document store goes last")
        #expect(try index("ConflictDrafts") < index("runtime"))
        #expect(try index("store.pre-rust-aa.json") < index("store.json"))
        #expect(removed.last == "store.json")
        #expect(try String(contentsOf: lane.markerURL, encoding: .utf8) == "reset=done\n")
    }

    @Test("027-FR-021: running the reset again changes nothing and never reopens a finished one")
    func idempotent() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        for artifact in LegacyArtifacts.all { try lane.plant(artifact) }
        let reset = lane.reset()
        try await reset.confirm()
        try await reset.confirm()
        #expect(lane.finished.withLock { $0 } == 1, "a finished reset does not run its steps again")
        #expect(try await lane.reset().resume() == false)
        #expect(try await lane.reset().phase() == .done)
        #expect(await lane.reset().needsConfirmation() == false)
    }

    @Test("027-FR-021: an unrecognised marker is not consent to delete")
    func unrecognisedMarkerIsNotConsent() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        try lane.write(Data("reset=maybe\n".utf8), lane.layout.documentFolder, "fresh-start")
        let reset = lane.reset()
        #expect(try await reset.phase() == .notStarted)
        #expect(try await reset.resume() == false)
    }

    // MARK: Interruption and disk errors

    @Test("027-FR-021: a kill at any point after confirmation resumes on the next launch without asking", arguments: 0..<12)
    func resumesAfterKill(killedAtRemoval: Int) async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        for artifact in LegacyArtifacts.all { try lane.plant(artifact) }
        let first = lane.reset(files: RecordingFiles(failRemovalNumber: killedAtRemoval))
        let killed = (try? await first.confirm()) == nil
        if killed {
            #expect(try await first.phase() == .confirmed)
            #expect(await first.networkAllowed() == false)
        }

        // The next launch: a new engine over the same disk.
        let next = lane.reset()
        #expect(try await next.phase() == (killed ? .confirmed : .done))
        #expect(await next.networkAllowed() == !killed)
        #expect(try await next.resume() == killed, "it resumes only an interrupted reset, and does not ask")
        for artifact in LegacyArtifacts.all { #expect(try !lane.isPresent(artifact), "\(artifact.id)") }
        #expect(try await next.phase() == .done)
        #expect(await next.networkAllowed())
    }

    @Test("027-FR-021: a disk error leaves the app blocked at confirmed and nothing may use the new server")
    func diskErrorBlocks() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        for artifact in LegacyArtifacts.all { try lane.plant(artifact) }
        let files = RecordingFiles(failingNames: ["runtime"])
        let reset = lane.reset(files: files)

        await #expect(throws: FreshStartResetError.stepFailed("runtime")) { try await reset.confirm() }
        #expect(try await reset.phase() == .confirmed)
        #expect(await reset.networkAllowed() == false)
        #expect(lane.finished.withLock { $0 } == 0)
        // "Try again" with the disk still failing stays blocked; it does not write done.
        await #expect(throws: FreshStartResetError.stepFailed("runtime")) { try await reset.resume() }
        #expect(try String(contentsOf: lane.markerURL, encoding: .utf8) == "reset=confirmed\n")

        files.failingNames.withLock { $0 = [] }
        #expect(try await reset.resume())
        #expect(try await reset.phase() == .done)
        #expect(await reset.networkAllowed())
    }

    @Test("027-FR-021: a Keychain error is a failed step, not a finished reset")
    func keychainErrorBlocks() async throws {
        let lane = try ResetLane(tokenStores: [FailingTokenStore()])
        defer { lane.cleanup() }
        let reset = lane.reset()
        await #expect(throws: FreshStartResetError.stepFailed("keychain.session")) { try await reset.proceedWithoutAsking() }
        #expect(try await reset.phase() == .confirmed)
        #expect(await reset.networkAllowed() == false)
    }

    @Test("027-FR-021: if the marker cannot be written nothing is deleted")
    func unwritableMarkerDeletesNothing() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        for artifact in LegacyArtifacts.all { try lane.plant(artifact) }
        let reset = lane.reset(files: RecordingFiles(failWrites: true))
        await #expect(throws: FreshStartResetError.markerNotSaved) { try await reset.confirm() }
        for artifact in LegacyArtifacts.all { #expect(try lane.isPresent(artifact), "\(artifact.id)") }
        #expect(try await reset.phase() == .notStarted)
    }

    @Test("027-FR-021: a file that comes back before done keeps the reset from finishing")
    func reappearingFileKeepsTheResetOpen() async throws {
        let lane = try ResetLane()
        defer { lane.cleanup() }
        let files = RecordingFiles()
        // A writer (a widget, an App Intent) recreates the document while the later steps run.
        files.afterRemoving.withLock {
            $0 = { name in
                if name == "store.json" { try? Data("{}".utf8).write(to: lane.documentURL) }
            }
        }
        let reset = lane.reset(files: files)
        try lane.write(Data("{}".utf8), lane.layout.documentFolder, "store.json")
        await #expect(throws: FreshStartResetError.incomplete(["store.document"])) { try await reset.confirm() }
        #expect(try await reset.phase() == .confirmed)
        files.afterRemoving.withLock { $0 = nil }
        #expect(try await reset.resume())
        #expect(!FileManager.default.fileExists(atPath: lane.documentURL.path))
    }

    @Test("027-FR-021: the real shared HTTP state is cleared by the default")
    func defaultHTTPStateIsCleared() throws {
        let cookie = try #require(
            HTTPCookie(properties: [
                .domain: "brain-buddy.example", .path: "/", .name: "brainbuddy_session", .value: "old",
            ]))
        HTTPCookieStorage.shared.setCookie(cookie)
        FreshStartEnvironment.clearSharedHTTPState()
        #expect(HTTPCookieStorage.shared.cookies?.contains { $0.name == "brainbuddy_session" } != true)
    }

    // MARK: The inventory covers every path the legacy code writes

    /// Every source file that writes to disk, to `UserDefaults`, to `@AppStorage`, to the Keychain
    /// or to a cache is either an artifact of the inventory or exempt, with a reason. A new writer
    /// fails this test until somebody decides which. The app and Mac sources are only scanned
    /// where the checkout has them (the Linux container mounts the package alone).
    @Test("027-FR-021: the inventory covers every path the legacy code writes")
    func inventoryCoversEveryWriter() throws {
        let here = URL(fileURLWithPath: #filePath)
        let package = here.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        let repo = package.deletingLastPathComponent().deletingLastPathComponent()
        let roots = [
            "ios/BrainBuddyKit/Sources", "ios/BrainBuddy", "ios/Shared", "ios/BrainBuddyWidgets", "macos/Sources",
        ]
        let tokens = [
            ".write(to:", "AppStorage", "UserDefaults", "SecItemAdd", "createDirectory(", "O_CREAT", "createFile",
            "FileHandle(forWriting", "HTTPCookieStorage", "URLCache",
        ]
        var found: [String] = []
        for root in roots {
            let base = repo.appendingPathComponent(root)
            guard let walker = FileManager.default.enumerator(atPath: base.path) else { continue }
            for case let relative as String in walker where relative.hasSuffix(".swift") {
                let path = "\(root)/\(relative)"
                if path.contains("BrainBuddyRust") || path.contains("BrainBuddyDesign/") { continue }
                let text = try String(contentsOf: base.appendingPathComponent(relative), encoding: .utf8)
                if tokens.contains(where: text.contains) { found.append(path) }
            }
        }
        #expect(!found.isEmpty)
        for path in found {
            #expect(LegacyWriters.decisions[path] != nil, "\(path) writes data: add it to the inventory or exempt it")
        }
        let ids = Set(LegacyArtifacts.all.map(\.id))
        let claimed = Set(LegacyWriters.decisions.values.compactMap { $0.artifacts }.flatMap { $0 })
        #expect(claimed.isSubset(of: ids), "unknown artifact ids: \(claimed.subtracting(ids))")
        #expect(
            ids.subtracting(LegacyWriters.defensive).isSubset(of: claimed),
            "artifacts no writer is mapped to: \(ids.subtracting(claimed))")
    }
}

// MARK: - Test support

/// One temporary install: the document and support folders, a defaults suite and a token store.
final class ResetLane: Sendable {
    let root: URL
    let layout: FreshStartLayout
    let suite = "bb.freshstart.\(UUID().uuidString)"
    let tokens: any SessionTokenStore
    let tokenStores: [any SessionTokenStore]
    let httpCleared = Mutex(false)
    let finished = Mutex(0)

    var documentFolder: URL { layout.documentFolder }
    var documentURL: URL { documentFolder.appendingPathComponent("store.json") }
    var markerURL: URL { documentFolder.appendingPathComponent("fresh-start") }

    init(tokenStores: [any SessionTokenStore]? = nil) throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("freshstart-\(UUID().uuidString)")
        layout = FreshStartLayout(
            documentFolder: root.appendingPathComponent("group/BrainBuddy", isDirectory: true),
            supportFolder: root.appendingPathComponent("own/BrainBuddy", isDirectory: true))
        let memory = InMemorySessionTokenStore()
        tokens = memory
        self.tokenStores = tokenStores ?? [memory]
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    func reset(files: (any FreshStartFileSystem)? = nil) -> FreshStartReset {
        FreshStartReset(
            environment: FreshStartEnvironment(
                layout: layout, files: files ?? DiskFreshStartFileSystem(), defaultsSuites: [suite],
                tokenStores: tokenStores, clearHTTPState: { [self] in httpCleared.withLock { $0 = true } },
                didFinish: { [self] in finished.withLock { $0 += 1 } }))
    }

    func cleanup() {
        try? FileManager.default.removeItem(at: root)
        UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite)
    }

    func write(_ data: Data, _ folder: URL, _ components: String...) throws {
        var url = folder
        for component in components.dropLast() { url.appendPathComponent(component, isDirectory: true) }
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        try data.write(to: url.appendingPathComponent(components[components.count - 1]))
    }

    private func folder(_ folder: LegacyArtifactLocation.Folder) -> URL {
        folder == .document ? layout.documentFolder : layout.supportFolder
    }

    /// Creates something at the artifact's location, by the test's own reading of the location.
    func plant(_ artifact: LegacyArtifact) throws {
        let data = Data("x".utf8)
        let defaults = try #require(UserDefaults(suiteName: suite))
        switch artifact.location {
        case .file(let where_, let name): try write(data, folder(where_), name)
        case .directory(let where_, let name): try write(data, folder(where_), name, "inner.json")
        case .siblings(let where_, let prefix, let suffix):
            try write(data, folder(where_), prefix + "aa" + suffix)
        case .runtimeFolder:
            try write(data, layout.documentFolder, "runtime", "selection.json")
            try write(data, layout.documentFolder, "runtime", "6162", "store.sqlite3")
        case .defaultsKey(let key): defaults.set("v", forKey: key)
        case .defaultsKeyPrefix(let prefix): defaults.set("v", forKey: prefix + "list.next")
        case .sessionTokens: try tokens.setToken("old", for: Fixture.serverURL)
        case .pendingLogouts:
            try tokens.addPendingLogout(
                PendingLogout(serverURL: Fixture.serverURL, token: "old", signedOutAt: Fixture.epoch))
        case .httpCaches: httpCleared.withLock { $0 = false }
        }
    }

    /// Whether the artifact is still there, by the test's own reading of the location.
    func isPresent(_ artifact: LegacyArtifact) throws -> Bool {
        let manager = FileManager.default
        let defaults = try #require(UserDefaults(suiteName: suite))
        switch artifact.location {
        case .file(let where_, let name), .directory(let where_, let name):
            return manager.fileExists(atPath: folder(where_).appendingPathComponent(name).path)
        case .siblings(let where_, let prefix, let suffix):
            let names = (try? manager.contentsOfDirectory(atPath: folder(where_).path)) ?? []
            return names.contains { $0.hasPrefix(prefix) && $0.hasSuffix(suffix) }
        case .runtimeFolder: return manager.fileExists(atPath: layout.documentFolder.appendingPathComponent("runtime").path)
        case .defaultsKey(let key): return defaults.object(forKey: key) != nil
        case .defaultsKeyPrefix(let prefix): return defaults.dictionaryRepresentation().keys.contains { $0.hasPrefix(prefix) }
        case .sessionTokens: return try tokens.token(for: Fixture.serverURL) != nil
        case .pendingLogouts: return try !tokens.pendingLogouts().isEmpty
        case .httpCaches: return !httpCleared.withLock { $0 }
        }
    }
}

/// The real disk, with a log of what the engine wrote and removed, and faults to inject.
final class RecordingFiles: FreshStartFileSystem {
    private let disk = DiskFreshStartFileSystem()
    let log = Mutex<[String]>([])
    let failingNames: Mutex<Set<String>>
    /// The removal (counted from 0) that fails once, as a kill would stop the process.
    private let failRemovalNumber: Int?
    private let removals = Mutex(0)
    private let failWrites: Bool
    let afterRemoving = Mutex<(@Sendable (String) -> Void)?>(nil)

    init(failingNames: Set<String> = [], failRemovalNumber: Int? = nil, failWrites: Bool = false) {
        self.failingNames = Mutex(failingNames)
        self.failRemovalNumber = failRemovalNumber
        self.failWrites = failWrites
    }

    func exists(_ url: URL) -> Bool { disk.exists(url) }
    func names(in folder: URL) -> [String] { disk.names(in: folder) }
    func read(_ url: URL) throws -> Data? { try disk.read(url) }

    func remove(_ url: URL) throws {
        let name = url.lastPathComponent
        let number = removals.withLock { count in
            defer { count += 1 }
            return count
        }
        if failingNames.withLock({ $0.contains(name) }) || number == failRemovalNumber {
            throw CocoaError(.fileWriteUnknown)
        }
        log.withLock { $0.append("remove:\(name)") }
        try disk.remove(url)
        afterRemoving.withLock { $0 }?(name)
    }

    func write(_ data: Data, to url: URL) throws {
        if failWrites { throw CocoaError(.fileWriteOutOfSpace) }
        let text = String(decoding: data, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
        log.withLock { $0.append("write:\(url.lastPathComponent):\(text)") }
        try disk.write(data, to: url)
    }
}

/// A Keychain that refuses to forget.
struct FailingTokenStore: SessionTokenStore {
    struct Refused: Error {}
    func token(for serverURL: URL) throws -> String? { nil }
    func setToken(_ token: String, for serverURL: URL) throws {}
    func removeToken(for serverURL: URL) throws {}
    func removeAllTokens() throws { throw Refused() }
}

/// Which artifact (or exemption) each source file that writes data belongs to. Reasons are the
/// review: a writer that is not here fails `inventoryCoversEveryWriter`.
enum LegacyWriters {
    struct Decision {
        var artifacts: [String]?
        var exemption: String?
        static func covers(_ ids: String...) -> Decision { Decision(artifacts: ids) }
        static func exempt(_ reason: String) -> Decision { Decision(exemption: reason) }
    }

    /// Cleared though nothing in the source writes them: both HTTP stacks are ephemeral sessions, but
    /// the spec lists the shared caches and cookie jar, which the system and web views may fill.
    static let defensive: Set<String> = ["http.cachesAndCookies"]

    static let decisions: [String: Decision] = [
        "ios/BrainBuddyKit/Sources/BrainBuddyAPI/SessionTokenStore.swift": .covers("keychain.session", "keychain.pendingLogouts"),
        "ios/BrainBuddyKit/Sources/BrainBuddyPersistence/ConflictDraftStore.swift":
            .covers("drafts.conflict.document", "drafts.conflict.support"),
        "ios/BrainBuddyKit/Sources/BrainBuddyPersistence/DocumentFile.swift":
            .covers("store.document", "store.lock", "store.temporary", "store.quarantine"),
        "ios/BrainBuddyKit/Sources/BrainBuddyWorkspace/ReviewSuggestionModel.swift": .covers("drafts.reviewSuggestions"),
        "ios/BrainBuddyKit/Sources/BrainBuddyWorkspace/RustRuntimeLauncher.swift": .covers("runtime", "store.backups"),
        "ios/BrainBuddyKit/Sources/BrainBuddyWorkspace/RustRuntimeSelectionStore.swift": .covers("runtime", "defaults.pilotSwitch"),
        "ios/BrainBuddy/Screens/Account/AccountSignOut.swift": .covers("defaults.recentSearches"),
        "ios/BrainBuddy/Screens/Search/SearchScreen.swift": .covers("defaults.recentSearches"),
        "ios/BrainBuddy/Screens/Lists/TaskListScreen.swift": .covers("defaults.listOptions", "defaults.reviewNote"),
        // The exemptions: each is a decision that the data is not the earlier server's, or not data.
        "ios/BrainBuddyKit/Sources/BrainBuddyWorkspace/LegacyArtifacts.swift": .exempt("the inventory names URLCache"),
        "ios/BrainBuddyKit/Sources/BrainBuddyWorkspace/FreshStartReset.swift": .exempt("the reset itself"),
        "ios/BrainBuddyKit/Sources/BrainBuddyWorkspace/PhoneFirstRunFlow.swift": .exempt("first-run record: onboarding state, no content"),
        "ios/BrainBuddy/App/BrainBuddyApp.swift": .exempt("hands the first-run record store to the flow"),
        "ios/BrainBuddy/App/RootView.swift": .exempt("reads the -BBSkipFirstRun launch argument"),
        "ios/BrainBuddyKit/Sources/BrainBuddyDiagnostics/DiagnosticsStore.swift": .exempt("performance log: no user content"),
        "ios/BrainBuddy/App/PerformanceDiagnostics.swift": .exempt("performance log: no user content"),
        "ios/BrainBuddyKit/Sources/BrainBuddyDesignTestSupport/SnapshotAssertion.swift": .exempt("test-only snapshots"),
        "ios/BrainBuddy/Screens/Account/AccountScreen.swift": .exempt("the data export, written where the person chose"),
        "macos/Sources/BrainBuddyMacCore/AccountExportFlow.swift": .exempt("the data export, written where the person chose"),
        "ios/Shared/SharedWorkspace.swift": .exempt("one-byte device-lock probe"),
        "macos/Sources/BrainBuddyMacCore/SingleInstanceGuard.swift": .exempt("process lock file"),
        "macos/Sources/BrainBuddyMacCore/WhisperModelFiles.swift": .exempt("downloaded speech model"),
        "macos/Sources/BrainBuddyMacCore/WorkspaceHost.swift": .exempt("reads the configured server address, a setting"),
        "macos/Sources/BrainBuddyMacCore/MacFileSystem.swift": .exempt("file helpers; mac-local.json is device state, see MacLocalState"),
        "macos/Sources/BrainBuddyMacCore/MacLocalState.swift":
            .exempt("mac-local.json: review marks keyed by digests, sidebar and first run; never sent, no content"),
    ]
}
