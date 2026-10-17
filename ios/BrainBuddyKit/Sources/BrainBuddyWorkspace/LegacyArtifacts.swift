import BrainBuddyPersistence
import Foundation

// The committed inventory of what the fresh start deletes (spec 027, FR-021, data-model "Device
// stores"): everything the retired runtime and the 026 pilot left on an install, in the order the
// reset removes it. `FreshStartResetTests` plants each entry and asserts it is gone afterwards, and
// scans the source for every file that writes data, so a new writer has to be added here or exempted
// with a reason. Names come from the writers' own constants where those are reachable.

/// Where one artifact lives. Folders are the host's (`FreshStartLayout`); defaults are the app's.
public enum LegacyArtifactLocation: Hashable, Sendable {
    /// The folder of `store.json`: the App Group's on the iPhone, `BrainBuddyMac` on the Mac.
    /// `support` is the app's own `Application Support/BrainBuddy` (the same folder without an App Group).
    public enum Folder: Sendable { case document, support }

    case file(Folder, String)
    case directory(Folder, String)
    /// Files of a folder whose names start with `prefix` and end with `suffix`.
    case siblings(Folder, prefix: String, suffix: String)
    /// `runtime/` beside the document: the Rust client SQLite of every workspace identity, the
    /// runtime selection, the import marker and the sync cursor and hint state they hold.
    case runtimeFolder
    case defaultsKey(String)
    case defaultsKeyPrefix(String)
    /// The session tokens of every server in the Keychain.
    case sessionTokens
    /// Logouts that could not reach the old server, with the token each carries.
    case pendingLogouts
    /// `URLCache` and the HTTP cookie jar.
    case httpCaches
}

public struct LegacyArtifact: Hashable, Sendable {
    /// Stable name; the failing step is reported by it.
    public let id: String
    public let location: LegacyArtifactLocation
    /// What the artifact holds, for the people who maintain the list.
    public let holds: String
}

public enum LegacyArtifacts {
    /// Deletion order. Sessions and caches first, then the drafts, then the runtime's stores, and the
    /// legacy document last: its absence is what makes a later launch see an empty install.
    public static let all: [LegacyArtifact] = [
        LegacyArtifact(id: "keychain.session", location: .sessionTokens, holds: "old-session tokens"),
        LegacyArtifact(id: "keychain.pendingLogouts", location: .pendingLogouts, holds: "unsent logouts of old sessions"),
        LegacyArtifact(id: "http.cachesAndCookies", location: .httpCaches, holds: "URLCache, cookie state"),
        LegacyArtifact(
            id: "drafts.conflict.document", location: .directory(.document, conflictDrafts),
            holds: "conflict drafts (the Mac keeps them in its folder)"),
        LegacyArtifact(
            id: "drafts.conflict.support", location: .directory(.support, conflictDrafts),
            holds: "conflict drafts (the iPhone keeps them in its own container)"),
        LegacyArtifact(
            id: "drafts.reviewSuggestions", location: .directory(.support, reviewSuggestions),
            holds: "typed answers and cancellation markers of Review suggestions"),
        LegacyArtifact(
            id: "runtime", location: .runtimeFolder,
            holds: "pre-Rust and Rust client SQLite, selection, import-pending marker, sync cursor and hints"),
        LegacyArtifact(
            id: "store.backups", location: .siblings(.document, prefix: "store.pre-rust-", suffix: ".json"),
            holds: "026 import reports (manifests) and the retained backup of the imported store"),
        LegacyArtifact(
            id: "store.quarantine", location: .siblings(.document, prefix: "store.unreadable-", suffix: ".json"),
            holds: "documents set aside as unreadable"),
        LegacyArtifact(
            id: "store.temporary", location: .siblings(.document, prefix: ".store.json.", suffix: ".tmp"),
            holds: "leftovers of interrupted document writes"),
        LegacyArtifact(id: "store.lock", location: .file(.document, ".store.json.lock"), holds: "the writers' lock file"),
        LegacyArtifact(
            id: "store.document", location: .file(.document, FileDocumentStore.defaultFileName),
            holds: "tasks, outbox and replay journal, drafts, issues, sync cursor; what the widgets read"),
        LegacyArtifact(
            id: "defaults.pilotSwitch", location: .defaultsKey(RustRuntimePilotSwitch.defaultsKey),
            holds: "the 026 pilot switch"),
        LegacyArtifact(
            id: "defaults.recentSearches", location: .defaultsKey("search.recentQueries"),
            holds: "recent searches (`RecentSearches.storageKey`, in the app target)"),
        LegacyArtifact(
            id: "defaults.listOptions", location: .defaultsKeyPrefix("listOptions."),
            holds: "list options per destination, named by project and tag ids (`ListOptionsStore`)"),
        LegacyArtifact(
            id: "defaults.reviewNote", location: .defaultsKey("review.thresholdNoteDismissedAt"),
            holds: "the dismissed Review threshold note"),
    ]

    private static var conflictDrafts: String { FileConflictDraftStore.applicationSupport().directory.lastPathComponent }
    private static var reviewSuggestions: String { FileReviewSuggestionJournal.standard.directory.lastPathComponent }
}
