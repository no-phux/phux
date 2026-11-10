import Foundation
import NIOCore
import NIOPosix
import NIOSSH

// The phone's one SSH use (ADR-0065): log in to a computer on the person's
// tailnet and run the POSIX scripts that locate and pair existing phux there.
// Exec only — no PTY, no shell, no forwarding — over SwiftNIO SSH.
//
// Authentication tries `none` first, which is how Tailscale SSH admits a
// tailnet identity, then the password the person typed, if any. A
// Tailscale SSH "check" policy holds `none` open behind a banner naming a
// login URL; the banner reaches the caller so it can show that URL.

/// Who to log in as and where.
struct SSHTarget: Sendable, Equatable {
    public var host: String
    public var port: Int
    public var username: String

    public init(host: String, port: Int = 22, username: String) {
        self.host = host
        self.port = port
        self.username = username
    }
}

/// Which host keys the session accepts.
enum SSHHostKeyPolicy: Sendable, Equatable {
    /// Only these OpenSSH-format keys (`ssh-ed25519 AAAA…`), as the tailnet's
    /// own map reports them for a Tailscale SSH node.
    case pinned([String])
    /// Any key: the address is a tailnet node reached through this phone's
    /// WireGuard tunnel, which already authenticated the peer's node key.
    case tailnetPeer
}

/// What one remote command produced.
struct SSHCommandResult: Sendable, Equatable {
    public var exitStatus: Int?
    public var stdout: String
    public var stderr: String

    public init(exitStatus: Int?, stdout: String, stderr: String) {
        self.exitStatus = exitStatus
        self.stdout = stdout
        self.stderr = stderr
    }

    public var succeeded: Bool { exitStatus == 0 }
}

/// Why an SSH session did not run a command.
public enum SSHFailure: Error, Sendable, Equatable, LocalizedError {
    /// Nothing answered at the address (refused, unreachable, timed out).
    case unreachable(String)
    /// The host key matched none the tailnet reported for this computer.
    case hostKeyMismatch
    /// Every method was tried and refused. `passwordAccepted` says whether
    /// the server would take a password at all.
    case authenticationFailed(passwordAccepted: Bool)
    /// The server refused to run the command, or closed mid-command.
    case commandRefused
    /// No answer before the deadline.
    case timedOut
    case outputTooLarge

    public var errorDescription: String? {
        switch self {
        case .unreachable: "SSH could not reach this computer. Check Tailscale and Remote Login."
        case .hostKeyMismatch: "The SSH host key did not match this computer's tailnet map."
        case .authenticationFailed(passwordAccepted: true): "SSH requires the computer account password."
        case .authenticationFailed(passwordAccepted: false): "SSH refused this account or authentication method."
        case .commandRefused: "SSH could not run the pairing command."
        case .timedOut: "SSH pairing timed out. Try again."
        case .outputTooLarge: "The remote command returned too much output."
        }
    }
}

/// A live, authenticated SSH connection.
final class SSHConnection: Sendable {
    private let channel: any Channel

    private init(channel: any Channel) {
        self.channel = channel
    }

    /// Connect and authenticate. `onBanner` receives any banner the server
    /// sends while authenticating (Tailscale SSH's check-mode login URL).
    public static func connect(
        to target: SSHTarget,
        password: String?,
        hostKeys: SSHHostKeyPolicy,
        authenticationTimeout: TimeAmount = .seconds(180),
        onBanner: @escaping @Sendable (String) -> Void = { _ in }
    ) async throws -> SSHConnection {
        let group = MultiThreadedEventLoopGroup.singleton
        let authenticated = group.next().makePromise(of: Void.self)
        let userAuth = OfferingUserAuth(username: target.username, password: password)
        let serverAuth = HostKeyCheck(policy: hostKeys)
        let attempt = SSHAttempt()
        let bootstrap = ClientBootstrap(group: group)
            .connectTimeout(.seconds(10))
            .channelOption(.socketOption(.tcp_nodelay), value: 1)
            .channelInitializer { channel in
                attempt.store(channel)
                return channel.eventLoop.makeCompletedFuture {
                    let ssh = NIOSSHHandler(
                        role: .client(.init(
                            userAuthDelegate: userAuth,
                            serverAuthDelegate: serverAuth
                        )),
                        allocator: channel.allocator,
                        inboundChildChannelInitializer: nil
                    )
                    try channel.pipeline.syncOperations.addHandler(ssh)
                    try channel.pipeline.syncOperations.addHandler(
                        AuthenticationWatch(
                            authenticated: authenticated, onBanner: onBanner,
                            failure: { serverAuth.rejected ? .hostKeyMismatch : userAuth.failure }
                        )
                    )
                }
            }

        let channel: any Channel
        do {
            channel = try await withTaskCancellationHandler {
                try Task.checkCancellation()
                return try await bootstrap.connect(host: target.host, port: target.port).get()
            } onCancel: { attempt.cancel() }
        } catch {
            authenticated.fail(error)
            if Task.isCancelled { throw CancellationError() }
            throw SSHFailure.unreachable("connection failed")
        }
        let deadline = channel.eventLoop.scheduleTask(in: authenticationTimeout) {
            authenticated.fail(SSHFailure.timedOut)
        }
        do {
            // A cancelled setup (the sheet closed) stops waiting at once,
            // including on a Tailscale SSH check nobody will confirm.
            try await withTaskCancellationHandler {
                try await authenticated.futureResult.get()
            } onCancel: {
                authenticated.fail(CancellationError())
            }
            deadline.cancel()
        } catch {
            deadline.cancel()
            try? await channel.close()
            if error is CancellationError { throw error }
            throw error as? SSHFailure ?? userAuth.failure
        }
        return SSHConnection(channel: channel)
    }

    /// Run `command` (handed to the login shell, as `ssh host command` is),
    /// writing `stdin` to it and closing its input. Output is decoded as UTF-8.
    public func run(
        _ command: String,
        stdin: String? = nil,
        timeout: TimeAmount = .seconds(300)
    ) async throws -> SSHCommandResult {
        let eventLoop = channel.eventLoop
        let result = eventLoop.makePromise(of: SSHCommandResult.self)
        let channelReady = eventLoop.makePromise(of: (any Channel).self)
        channelReady.futureResult.whenFailure { result.fail($0) }
        let input = stdin
        channel.pipeline.handler(type: NIOSSHHandler.self).whenComplete { lookup in
            switch lookup {
            case let .failure(error):
                channelReady.fail(error)
            case let .success(ssh):
                ssh.createChannel(channelReady, channelType: .session) { child, type in
                    guard type == .session else {
                        return child.eventLoop.makeFailedFuture(SSHFailure.commandRefused)
                    }
                    return child.eventLoop.makeCompletedFuture {
                        try child.pipeline.syncOperations.addHandler(
                            ExecHandler(command: command, stdin: input, result: result)
                        )
                    }
                }
            }
        }
        let parent = channel
        let deadline = eventLoop.scheduleTask(in: timeout) {
            channelReady.fail(SSHFailure.timedOut)
            result.fail(SSHFailure.timedOut)
            parent.close(promise: nil)
        }
        defer { deadline.cancel() }
        return try await withTaskCancellationHandler {
            _ = try await channelReady.futureResult.get()
            return try await result.futureResult.get()
        } onCancel: {
            channelReady.fail(CancellationError())
            result.fail(CancellationError())
            parent.close(promise: nil)
        }
    }

    public func close() async {
        try? await channel.close()
    }
}

/// Cancellation can arrive before ClientBootstrap has created its channel.
/// This lock covers only publication of that handle; all I/O stays on NIO.
private final class SSHAttempt: @unchecked Sendable {
    private let lock = NSLock()
    private var channel: (any Channel)?
    private var cancelled = false

    func store(_ channel: any Channel) {
        lock.lock()
        self.channel = channel
        let close = cancelled
        lock.unlock()
        if close { channel.close(promise: nil) }
    }

    func cancel() {
        lock.lock()
        cancelled = true
        let channel = channel
        lock.unlock()
        channel?.close(promise: nil)
    }
}

// MARK: - Delegates and handlers

/// Offers `none`, then the password when the server takes one. Lives on the
/// connection's event loop; NIOSSH calls it only there.
private final class OfferingUserAuth: NIOSSHClientUserAuthenticationDelegate, @unchecked Sendable {
    typealias Offer = NIOSSHUserAuthenticationOffer

    private let username: String
    private var password: String?
    private var offeredNone = false
    /// Whether the server, answering `none`, listed `password`. NIOSSH's
    /// first call passes every method before the server has said anything,
    /// so only later calls carry its answer.
    private(set) var passwordAccepted = false

    init(username: String, password: String?) {
        self.username = username
        self.password = password
    }

    /// Why the session ended before authenticating: refused, once a login
    /// was offered; otherwise the handshake itself failed.
    var failure: SSHFailure {
        offeredNone
            ? .authenticationFailed(passwordAccepted: passwordAccepted)
            : .unreachable("the SSH handshake failed")
    }

    func nextAuthenticationType(
        availableMethods: NIOSSHAvailableUserAuthenticationMethods,
        nextChallengePromise: EventLoopPromise<NIOSSHUserAuthenticationOffer?>
    ) {
        if offeredNone { passwordAccepted = availableMethods.contains(.password) }
        if !offeredNone {
            offeredNone = true
            nextChallengePromise.succeed(offer(.none))
            return
        }
        if let password, availableMethods.contains(.password) {
            self.password = nil
            nextChallengePromise.succeed(offer(.password(.init(password: password))))
            return
        }
        nextChallengePromise.fail(failure)
    }

    private func offer(_ offer: Offer.Offer) -> Offer {
        Offer(username: username, serviceName: "", offer: offer)
    }
}

/// Applies an ``SSHHostKeyPolicy`` and remembers a rejection.
private final class HostKeyCheck: NIOSSHClientServerAuthenticationDelegate, @unchecked Sendable {
    private let policy: SSHHostKeyPolicy
    private(set) var rejected = false

    init(policy: SSHHostKeyPolicy) {
        self.policy = policy
    }

    func validateHostKey(
        hostKey: NIOSSHPublicKey,
        validationCompletePromise: EventLoopPromise<Void>
    ) {
        guard SSHHostKeyPolicyCheck.accepts(String(openSSHPublicKey: hostKey), policy: policy)
        else {
            rejected = true
            validationCompletePromise.fail(SSHFailure.hostKeyMismatch)
            return
        }
        validationCompletePromise.succeed(())
    }
}

/// The pure host-key rule, testable without a server.
enum SSHHostKeyPolicyCheck {
    /// Whether `presented` (`<algorithm> <base64>`) satisfies `policy`.
    /// Pinned keys compare on algorithm and key, ignoring any comment.
    static func accepts(_ presented: String, policy: SSHHostKeyPolicy) -> Bool {
        switch policy {
        case .tailnetPeer:
            return true
        case let .pinned(keys):
            let wanted = keyIdentity(presented)
            return wanted != nil && keys.contains { keyIdentity($0) == wanted }
        }
    }

    private static func keyIdentity(_ openSSH: String) -> String? {
        let fields = openSSH.split(separator: " ", omittingEmptySubsequences: true)
        guard fields.count >= 2 else { return nil }
        return "\(fields[0]) \(fields[1])"
    }
}

/// Completes `authenticated` on success, fails it when the connection
/// errors or closes first, and relays banners.
private final class AuthenticationWatch: ChannelInboundHandler, @unchecked Sendable {
    typealias InboundIn = Any

    private let authenticated: EventLoopPromise<Void>
    private let onBanner: @Sendable (String) -> Void
    private let failure: () -> SSHFailure

    init(
        authenticated: EventLoopPromise<Void>,
        onBanner: @escaping @Sendable (String) -> Void,
        failure: @escaping () -> SSHFailure
    ) {
        self.authenticated = authenticated
        self.onBanner = onBanner
        self.failure = failure
    }

    func userInboundEventTriggered(context: ChannelHandlerContext, event: Any) {
        switch event {
        case is UserAuthSuccessEvent:
            authenticated.succeed(())
        case let banner as NIOUserAuthBannerEvent:
            onBanner(banner.message)
        default:
            context.fireUserInboundEventTriggered(event)
        }
    }

    func errorCaught(context: ChannelHandlerContext, error: any Error) {
        authenticated.fail(error as? SSHFailure ?? failure())
        context.close(promise: nil)
    }

    func channelInactive(context: ChannelHandlerContext) {
        authenticated.fail(failure())
        context.fireChannelInactive()
    }
}
