import Foundation
import TailscaleKit

// The tailnet directory (ADR-0065): an embedded Tailscale node, signed in as
// the person once, that reads the tailnet's map so onboarding can list their
// computers. iOS gives no app the system Tailscale client's peers, so this
// node is the only way to see them. It runs only while the picker is on
// screen, uploads no logs, and dials nothing: the SSH session that follows
// rides the phone's own Tailscale tunnel.

/// Lists the person's tailnet machines.
public protocol TailnetDirectory: Sendable {
    /// Start the node if needed and report its state until the stream's
    /// consumer stops listening, which stops the node.
    func snapshots() -> AsyncStream<TailnetSnapshot>
    /// Sign this node out and forget its state.
    func signOut() async
}

public actor EmbeddedTailnet: TailnetDirectory {
    private let stateDirectory: URL
    private let hostName: String
    private var node: TailscaleNode?
    private var listeners = 0
    /// Why the current node could not come up, once `up` says so.
    private var upFailure: String?
    /// A node shutting down still holds the state directory: the next
    /// start, and a sign-out's deletion, wait for it.
    private var closing: Task<Void, Never>?

    /// `hostName` is how the node appears in the person's admin console.
    public init(stateDirectory: URL = EmbeddedTailnet.defaultStateDirectory, hostName: String) {
        self.stateDirectory = stateDirectory
        self.hostName = hostName
    }

    /// Application Support, outside backups: a restored node key would be a
    /// second device impersonating this one.
    public static var defaultStateDirectory: URL {
        URL.applicationSupportDirectory.appending(path: "RuneTailnet", directoryHint: .isDirectory)
    }

    public nonisolated func snapshots() -> AsyncStream<TailnetSnapshot> {
        AsyncStream { continuation in
            let task = Task { await self.publish(into: continuation) }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public func signOut() async {
        await stop(forgettingState: true)
    }

    private func publish(into continuation: AsyncStream<TailnetSnapshot>.Continuation) async {
        listeners += 1
        defer { continuation.finish() }
        let node: TailscaleNode
        do {
            node = try await started()
        } catch {
            continuation.yield(TailnetSnapshot(phase: .stopped(String(describing: error))))
            await release()
            return
        }
        var last: TailnetSnapshot?
        while !Task.isCancelled, self.node === node {
            if let upFailure {
                continuation.yield(TailnetSnapshot(phase: .stopped(upFailure)))
                break
            }
            let snapshot = await Self.read(node)
            if snapshot != last {
                continuation.yield(snapshot)
                last = snapshot
            }
            let settled = snapshot.phase == .running
            try? await Task.sleep(for: settled ? .seconds(3) : .milliseconds(500))
        }
        await release()
    }

    private static func read(_ node: TailscaleNode) async -> TailnetSnapshot {
        do {
            return try await TailnetSnapshot.decode(node.statusJSON())
        } catch {
            return TailnetSnapshot(phase: .starting)
        }
    }

    /// The running node, created and brought up on first use. `up` blocks
    /// until the node is signed in, so it runs detached; the status poll
    /// reports the login URL meanwhile.
    private func started() async throws -> TailscaleNode {
        await closing?.value
        // Nothing below suspends until the node is stored, so concurrent
        // starts share one node.
        if let node { return node }
        try FileManager.default.createDirectory(
            at: stateDirectory,
            withIntermediateDirectories: true
        )
        var excluded = URLResourceValues()
        excluded.isExcludedFromBackup = true
        var directory = stateDirectory
        try directory.setResourceValues(excluded)
        _ = tailscale_disable_log_upload()
        let node = try TailscaleNode(
            config: Configuration(
                hostName: hostName,
                path: stateDirectory.path(percentEncoded: false),
                authKey: nil,
                controlURL: kDefaultControlURL,
                ephemeral: false
            ),
            logger: nil
        )
        self.node = node
        upFailure = nil
        Task.detached { [weak self] in
            do {
                try await node.up()
            } catch {
                await self?.upFailed(node, String(describing: error))
            }
        }
        return node
    }

    /// `up` failed for `failed`; a node this actor already closed is moot.
    private func upFailed(_ failed: TailscaleNode, _ reason: String) {
        guard node === failed else { return }
        upFailure = reason
    }

    /// Stop the node once nobody is watching it.
    private func release() async {
        listeners -= 1
        guard listeners == 0 else { return }
        await stop()
    }

    private func stop(forgettingState: Bool = false) async {
        let node = node
        self.node = nil
        let previous = closing
        let directory = stateDirectory
        let closing = Task {
            await previous?.value
            if forgettingState, let node {
                try? await LocalAPIClient(localNode: node, logger: nil).resetAuth()
            }
            try? await node?.close()
            if forgettingState { try? FileManager.default.removeItem(at: directory) }
        }
        self.closing = closing
        await closing.value
    }
}
