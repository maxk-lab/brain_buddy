import BrainBuddyAPI
import BrainBuddyCore
import BrainBuddyPersistence
import Foundation

#if canImport(FoundationNetworking)
    import FoundationNetworking
#endif

// The fresh-start reset engine (spec 027, FR-021, design M-02 / D-03). When an updated app first
// reaches the new server, everything the earlier runtime left on the install is deleted, but only
// after the person confirmed when it holds tasks, waiting changes or drafts. The deletion is not
// atomic across files, the Keychain, settings and the App Group, so it is bracketed by a durable
// marker: `reset=confirmed` is written before the first step, the steps (`LegacyArtifacts.all`) run in
// their fixed order and are idempotent, and `reset=done` is written only after a sweep found every
// artifact gone. A launch that finds `confirmed` resumes without asking again. Until `done`, nothing
// may use the new server (`networkAllowed`); this type makes no network request itself.

/// The folders one host keeps the legacy files in.
public struct FreshStartLayout: Sendable {
    public var documentFolder: URL
    public var supportFolder: URL

    public init(documentFolder: URL, supportFolder: URL) {
        self.documentFolder = documentFolder
        self.supportFolder = supportFolder
    }

    /// The iPhone: the App Group's folder and the app's own container.
    public static func appGroup(_ appGroupID: String) -> FreshStartLayout {
        FreshStartLayout(
            documentFolder: RustRuntimeLocation.legacyStoreURL(appGroupID: appGroupID).deletingLastPathComponent(),
            supportFolder: FileDocumentStore.applicationSupportDocumentURL().deletingLastPathComponent())
    }

    /// The Mac: its one folder (`MacHostConfiguration.directory`).
    public static func directory(_ folder: URL) -> FreshStartLayout {
        FreshStartLayout(
            documentFolder: folder,
            supportFolder: FileDocumentStore.applicationSupportDocumentURL().deletingLastPathComponent())
    }
}

/// The disk the engine works on; tests inject faults.
public protocol FreshStartFileSystem: Sendable {
    func exists(_ url: URL) -> Bool
    func names(in folder: URL) -> [String]
    /// nil when there is no such file.
    func read(_ url: URL) throws -> Data?
    /// Removes a file or folder tree.
    func remove(_ url: URL) throws
    /// Replaces the file atomically and flushes it (and its folder) to disk; creates the folder.
    func write(_ data: Data, to url: URL) throws
}

public struct DiskFreshStartFileSystem: FreshStartFileSystem {
    public init() {}

    public func exists(_ url: URL) -> Bool { FileManager.default.fileExists(atPath: url.path) }

    public func names(in folder: URL) -> [String] {
        (try? FileManager.default.contentsOfDirectory(atPath: folder.path)) ?? []
    }

    public func read(_ url: URL) throws -> Data? { exists(url) ? try Data(contentsOf: url) : nil }

    public func remove(_ url: URL) throws {
        if exists(url) { try FileManager.default.removeItem(at: url) }
    }

    public func write(_ data: Data, to url: URL) throws {
        let folder = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(
            at: folder, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try data.write(to: url, options: [.atomic])
        for target in [url, folder] {
            let handle = try FileHandle(forReadingFrom: target)
            defer { try? handle.close() }
            try? handle.synchronize()
        }
    }
}

/// What the confirmation names. A nil count is unknown (the store could not be read, or is the
/// Rust runtime's); unknown counts as content, so the person is asked.
public struct FreshStartCounts: Equatable, Sendable {
    public var tasks: Int?
    public var waitingChanges: Int?
    public var drafts: Int?

    public init(tasks: Int?, waitingChanges: Int?, drafts: Int?) {
        self.tasks = tasks
        self.waitingChanges = waitingChanges
        self.drafts = drafts
    }

    public var holdsContent: Bool { [tasks, waitingChanges, drafts].contains { ($0 ?? 1) > 0 } }
}

public enum FreshStartResetError: Error, Equatable, Sendable {
    /// The marker exists and cannot be read: nothing is touched.
    case markerUnreadable
    /// The marker could not be written: nothing was deleted (confirmed) or the reset stays open (done).
    case markerNotSaved
    /// There is earlier content and the person has not confirmed.
    case confirmationRequired
    /// A step failed (a disk error, a locked file, the Keychain); the reset stays at `confirmed`.
    case stepFailed(String)
    /// Every step ran but these artifacts are still there (a writer put them back).
    case incomplete([String])
}

public struct FreshStartEnvironment: Sendable {
    public var layout: FreshStartLayout
    public var files: any FreshStartFileSystem
    /// `UserDefaults` suites to clean; nil is the app's standard defaults.
    public var defaultsSuites: [String?]
    /// The Keychain stores of the old sessions (the iPhone's and, on the Mac, its own service).
    public var tokenStores: [any SessionTokenStore]
    public var clearHTTPState: @Sendable () -> Void
    /// Runs once, after `reset=done` (the host reloads its widgets).
    public var didFinish: @Sendable () -> Void
    /// The counts of a Rust-selected runtime, given its `runtime/` folder; nil leaves them unknown.
    public var rustCounts: (@Sendable (URL) async -> FreshStartCounts?)?

    public init(
        layout: FreshStartLayout, files: any FreshStartFileSystem = DiskFreshStartFileSystem(),
        defaultsSuites: [String?] = [nil], tokenStores: [any SessionTokenStore] = [],
        clearHTTPState: @escaping @Sendable () -> Void = FreshStartEnvironment.clearSharedHTTPState,
        didFinish: @escaping @Sendable () -> Void = {}, rustCounts: (@Sendable (URL) async -> FreshStartCounts?)? = nil
    ) {
        self.layout = layout
        self.files = files
        self.defaultsSuites = defaultsSuites
        self.tokenStores = tokenStores
        self.clearHTTPState = clearHTTPState
        self.didFinish = didFinish
        self.rustCounts = rustCounts
    }

    public static func clearSharedHTTPState() {
        URLCache.shared.removeAllCachedResponses()
        HTTPCookieStorage.shared.removeCookies(since: .distantPast)
    }
}

public actor FreshStartReset {
    public enum Phase: Equatable, Sendable { case notStarted, confirmed, done }

    static let markerName = "fresh-start"
    private let environment: FreshStartEnvironment

    public init(environment: FreshStartEnvironment) {
        self.environment = environment
    }

    public func phase() throws -> Phase {
        let data: Data?
        do { data = try environment.files.read(markerURL) } catch { throw FreshStartResetError.markerUnreadable }
        // Anything but our two lines is no consent to delete.
        switch data.map({ String(decoding: $0, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines) }) {
        case "reset=confirmed": return .confirmed
        case "reset=done": return .done
        default: return .notStarted
        }
    }

    /// What the earlier install holds. Deletes and writes nothing.
    public func counts() async -> FreshStartCounts {
        var drafts = LegacyArtifacts.all.reduce(0) { total, artifact in
            guard artifact.id.hasPrefix("drafts."), case .directory(let folder, let name) = artifact.location else { return total }
            return total + environment.files.names(in: url(folder).appendingPathComponent(name)).filter { $0.hasSuffix(".json") }.count
        }
        if environment.files.exists(RustRuntimeLocation(root: runtimeURL).selectionURL) {
            // The Rust runtime's store is the install's; its counts need the bridge.
            let rust = await environment.rustCounts?(runtimeURL)
            return FreshStartCounts(tasks: rust?.tasks, waitingChanges: rust?.waitingChanges, drafts: rust?.drafts.map { $0 + drafts })
        }
        guard environment.files.exists(documentURL) else {
            return FreshStartCounts(tasks: 0, waitingChanges: 0, drafts: drafts)
        }
        guard let document = try? await FileDocumentStore(fileURL: documentURL).load() else {
            return FreshStartCounts(tasks: nil, waitingChanges: nil, drafts: nil)
        }
        drafts += document.local.formDrafts.count
        return FreshStartCounts(tasks: document.replayed().state.tasks.count, waitingChanges: document.outbox.count, drafts: drafts)
    }

    /// Whether the confirmation (.01) is due: a reset not yet confirmed and earlier content to lose.
    public func needsConfirmation() async -> Bool {
        guard (try? phase()) == .notStarted else { return false }
        return await counts().holdsContent
    }

    /// Nothing may use the new server (sign-in, sync, requests) until this is true.
    public func networkAllowed() -> Bool { (try? phase()) == .done }

    /// "Delete and continue": writes `reset=confirmed`, runs every step, writes `reset=done`.
    /// A finished reset is left as it is; an interrupted one continues.
    public func confirm() throws {
        switch try phase() {
        case .done: return
        case .confirmed: break
        case .notStarted: try writeMarker("reset=confirmed")
        }
        try run()
    }

    /// .08: earlier data with nothing the person wrote is removed without asking.
    public func proceedWithoutAsking() async throws {
        if try phase() == .notStarted, await counts().holdsContent { throw FreshStartResetError.confirmationRequired }
        try confirm()
    }

    /// The next launch (.06) and "Try again" (.05): continues a confirmed reset without asking.
    /// Returns false when there was nothing to resume.
    @discardableResult
    public func resume() throws -> Bool {
        guard try phase() == .confirmed else { return false }
        try run()
        return true
    }

    private func run() throws {
        for artifact in LegacyArtifacts.all {
            do { try remove(artifact) } catch { throw FreshStartResetError.stepFailed(artifact.id) }
        }
        let remaining = LegacyArtifacts.all.filter(isPresent)
        guard remaining.isEmpty else { throw FreshStartResetError.incomplete(remaining.map(\.id)) }
        try writeMarker("reset=done")
        environment.didFinish()
    }

    private func writeMarker(_ line: String) throws {
        do { try environment.files.write(Data((line + "\n").utf8), to: markerURL) } catch { throw FreshStartResetError.markerNotSaved }
    }

    private var markerURL: URL { environment.layout.documentFolder.appendingPathComponent(Self.markerName) }
    private var documentURL: URL { url(.document).appendingPathComponent(FileDocumentStore.defaultFileName) }
    private var runtimeURL: URL { RustRuntimeLocation.directory(environment.layout.documentFolder).root }

    private func url(_ folder: LegacyArtifactLocation.Folder) -> URL {
        folder == .document ? environment.layout.documentFolder : environment.layout.supportFolder
    }

    private func defaults() -> [UserDefaults] {
        environment.defaultsSuites.compactMap { $0.flatMap(UserDefaults.init(suiteName:)) ?? .standard }
    }

    /// The files an artifact names that exist now.
    private func paths(_ location: LegacyArtifactLocation) -> [URL] {
        let files = environment.files
        switch location {
        case .file(let folder, let name), .directory(let folder, let name):
            return [url(folder).appendingPathComponent(name)].filter(files.exists)
        case .siblings(let folder, let prefix, let suffix):
            return files.names(in: url(folder)).filter { $0.hasPrefix(prefix) && $0.hasSuffix(suffix) }
                .map { url(folder).appendingPathComponent($0) }
        case .runtimeFolder: return [runtimeURL].filter(files.exists)
        default: return []
        }
    }

    private func remove(_ artifact: LegacyArtifact) throws {
        switch artifact.location {
        case .defaultsKey(let key): defaults().forEach { $0.removeObject(forKey: key) }
        case .defaultsKeyPrefix(let prefix):
            for store in defaults() { store.dictionaryRepresentation().keys.filter { $0.hasPrefix(prefix) }.forEach { store.removeObject(forKey: $0) } }
        case .sessionTokens: try environment.tokenStores.forEach { try $0.removeAllTokens() }
        case .pendingLogouts:
            for store in environment.tokenStores { try store.pendingLogouts().forEach(store.removePendingLogout) }
        case .httpCaches: environment.clearHTTPState()
        default: try paths(artifact.location).forEach(environment.files.remove)
        }
    }

    /// The sweep before `done`. What cannot be listed (caches, Keychain sessions) is trusted to its step.
    private func isPresent(_ artifact: LegacyArtifact) -> Bool {
        switch artifact.location {
        case .defaultsKey(let key): return defaults().contains { $0.object(forKey: key) != nil }
        case .defaultsKeyPrefix(let prefix): return defaults().contains { $0.dictionaryRepresentation().keys.contains { $0.hasPrefix(prefix) } }
        case .pendingLogouts: return environment.tokenStores.contains { ((try? $0.pendingLogouts())?.isEmpty ?? false) == false }
        case .sessionTokens, .httpCaches: return false
        default: return !paths(artifact.location).isEmpty
        }
    }
}
