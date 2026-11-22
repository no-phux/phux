import Foundation
import NIOCore
import NIOPosix
import NIOSSH
import XCTest
@testable import RuneTailnet

/// Real SSH wire exchanges against an in-process loopback fixture. It never
/// launches a shell, uses the installed phux, or touches a system SSH key.
@MainActor
final class SSHTests: XCTestCase {
    func testNoneAuthenticationAndExecStdinExitStatus() async throws {
        try await withServer(.none) { server in
            let connection = try await server.connect()
            let result = try await connection.run("fixture", stdin: "hello\n")
            await connection.close()
            XCTAssertEqual(result, SSHCommandResult(exitStatus: 7, stdout: "hello\n", stderr: "fixture stderr"))
            XCTAssertEqual(server.state.methods, ["none"])
        }
    }

    func testPasswordOnlyAfterNoneAndOnlyWhenExplicitlySupplied() async throws {
        try await withServer(.password) { server in
            do {
                _ = try await server.connect()
                XCTFail("passwordless authentication succeeded")
            } catch { XCTAssertEqual(error as? SSHFailure, .authenticationFailed(passwordAccepted: true)) }
            XCTAssertEqual(server.state.methods, ["none"])
            let connection = try await server.connect(password: "fixture-password")
            await connection.close()
            XCTAssertEqual(server.state.methods, ["none", "none", "password"])
        }
    }

    /// NIOSSH drops `keyboard-interactive` from a method list, so a server
    /// that wants it (alone, or after the password) lists nothing it knows.
    func testUnanswerableStepIsNotReportedAsAWrongPassword() async throws {
        for (mode, methods) in [(FixtureMode.noPassword, ["none"]), (.secondFactor, ["none", "password"])] {
            try await withServer(mode) { server in
                do {
                    _ = try await server.connect(password: "fixture-password")
                    XCTFail("authenticated without the step the server asked for")
                } catch { XCTAssertEqual(error as? SSHFailure, .authenticationFailed(passwordAccepted: false)) }
                XCTAssertEqual(server.state.methods, methods)
            }
        }
    }

    func testBadPinPreventsAuthentication() async throws {
        try await withServer(.password) { server in
            do {
                _ = try await server.connect(password: "fixture-password", pin: "ssh-ed25519 AAAAnot-the-key")
                XCTFail("accepted incorrect host key")
            } catch { XCTAssertEqual(error as? SSHFailure, .hostKeyMismatch) }
            XCTAssertTrue(server.state.methods.isEmpty)
        }
    }

    func testAuthenticationDeadlineClosesPendingSession() async throws {
        try await withServer(.holdAuthentication) { server in
            do {
                _ = try await server.connect(authenticationTimeout: .milliseconds(100))
                XCTFail("authentication deadline did not fire")
            } catch { XCTAssertEqual(error as? SSHFailure, .timedOut) }
        }
    }

    func testCancellingAuthenticationClosesPendingSession() async throws {
        try await withServer(.holdAuthentication) { server in
            let task = Task { try await server.connect() }
            try await Task.sleep(for: .milliseconds(100))
            task.cancel()
            do {
                _ = try await task.value
                XCTFail("cancelled authentication completed")
            } catch { XCTAssertTrue(error is CancellationError) }
        }
    }

    func testExecDeadlineAndCancellationCloseSession() async throws {
        try await withServer(.hangExec) { server in
            let connection = try await server.connect()
            do {
                _ = try await connection.run("fixture", timeout: .milliseconds(100))
                XCTFail("exec deadline did not fire")
            } catch { XCTAssertEqual(error as? SSHFailure, .timedOut) }
            await connection.close()
            let second = try await server.connect()
            let task = Task { try await second.run("fixture") }
            try await Task.sleep(for: .milliseconds(100))
            task.cancel()
            do {
                _ = try await task.value
                XCTFail("cancelled exec completed")
            } catch { XCTAssertTrue(error is CancellationError) }
            await second.close()
        }
    }

    func testOutputBoundAndRefusedCommand() async throws {
        for mode in [FixtureMode.largeOutput, .refuseExec] {
            try await withServer(mode) { server in
                let connection = try await server.connect()
                do {
                    _ = try await connection.run("fixture")
                    XCTFail("invalid command output succeeded")
                } catch {
                    XCTAssertEqual(error as? SSHFailure, mode == .largeOutput ? .outputTooLarge : .commandRefused)
                }
                await connection.close()
            }
        }
    }

    private func withServer(
        _ mode: FixtureMode, operation: (TestSSHServer) async throws -> Void
    ) async throws {
        let server = try await TestSSHServer.start(mode)
        do {
            try await operation(server)
            await server.close()
        } catch {
            await server.close()
            throw error
        }
    }
}

private enum FixtureMode: Sendable {
    case none, password, noPassword, secondFactor, holdAuthentication, hangExec, largeOutput, refuseExec
}

private final class FixtureState: @unchecked Sendable {
    private let lock = NSLock()
    private var recorded: [String] = []
    private var pending: [EventLoopPromise<NIOSSHUserAuthenticationOutcome>] = []
    private var channels: [any Channel] = []

    var methods: [String] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    func record(_ method: String) {
        lock.lock()
        defer { lock.unlock() }
        recorded.append(method)
    }

    func hold(_ promise: EventLoopPromise<NIOSSHUserAuthenticationOutcome>) {
        lock.lock()
        defer { lock.unlock() }
        pending.append(promise)
    }

    func track(_ channel: any Channel) {
        lock.lock()
        defer { lock.unlock() }
        channels.append(channel)
    }

    func stop() -> [any Channel] {
        lock.lock()
        let promises = pending
        pending = []
        let handles = channels
        channels = []
        lock.unlock()
        for promise in promises { promise.fail(CancellationError()) }
        return handles
    }
}

private struct TestSSHServer: Sendable {
    let channel: any Channel
    let pin: String
    let state: FixtureState

    static func start(_ mode: FixtureMode) async throws -> Self {
        let key = NIOSSHPrivateKey(ed25519Key: .init())
        let state = FixtureState()
        let server = try await ServerBootstrap(group: MultiThreadedEventLoopGroup.singleton)
            .childChannelInitializer { channel in
                state.track(channel)
                return channel.eventLoop.makeCompletedFuture {
                    try channel.pipeline.syncOperations.addHandler(NIOSSHHandler(
                        role: .server(.init(hostKeys: [key], userAuthDelegate: FixtureAuth(mode: mode, state: state))),
                        allocator: channel.allocator,
                        inboundChildChannelInitializer: { child, type in
                            guard type == .session else {
                                return child.eventLoop.makeFailedFuture(SSHFailure.commandRefused)
                            }
                            return child.eventLoop.makeCompletedFuture {
                                try child.pipeline.syncOperations.addHandler(FixtureExec(mode: mode))
                            }
                        }
                    ))
                    try channel.pipeline.syncOperations.addHandler(FixtureErrors())
                }
            }.bind(host: "127.0.0.1", port: 0).get()
        return Self(channel: server, pin: String(openSSHPublicKey: key.publicKey), state: state)
    }

    func connect(
        password: String? = nil, pin: String? = nil,
        authenticationTimeout: TimeAmount = .seconds(3)
    ) async throws -> SSHConnection {
        try await SSHConnection.connect(
            to: SSHTarget(host: "127.0.0.1", port: channel.localAddress!.port!, username: "fixture"),
            password: password, hostKeys: .pinned([pin ?? self.pin]), authenticationTimeout: authenticationTimeout
        )
    }

    func close() async {
        try? await channel.close()
        for handle in state.stop() { try? await handle.close() }
    }
}

private final class FixtureAuth: NIOSSHServerUserAuthenticationDelegate {
    let mode: FixtureMode
    let state: FixtureState
    var supportedAuthenticationMethods: NIOSSHAvailableUserAuthenticationMethods {
        mode == .noPassword ? [] : .password
    }

    init(mode: FixtureMode, state: FixtureState) {
        self.mode = mode
        self.state = state
    }

    func requestReceived(
        request: NIOSSHUserAuthenticationRequest,
        responsePromise: EventLoopPromise<NIOSSHUserAuthenticationOutcome>
    ) {
        switch request.request {
        case .none: state.record("none")
        case .password: state.record("password")
        default: state.record("unexpected")
        }
        if mode == .holdAuthentication {
            state.hold(responsePromise)
            return
        }
        if mode == .noPassword {
            responsePromise.succeed(.failure)
            return
        }
        if mode != .password, mode != .secondFactor {
            responsePromise.succeed(.success)
            return
        }
        if case let .password(password) = request.request, password.password == "fixture-password" {
            responsePromise.succeed(mode == .secondFactor ? .partialSuccess(remainingMethods: []) : .success)
        } else {
            responsePromise.succeed(.failure)
        }
    }
}

private final class FixtureErrors: ChannelInboundHandler, Sendable {
    typealias InboundIn = Any
    func errorCaught(context: ChannelHandlerContext, error: any Error) { context.close(promise: nil) }
}

private final class FixtureExec: ChannelInboundHandler, @unchecked Sendable {
    typealias InboundIn = SSHChannelData
    let mode: FixtureMode
    private var input = ByteBuffer()

    init(mode: FixtureMode) { self.mode = mode }

    func handlerAdded(context: ChannelHandlerContext) {
        _ = context.channel.setOption(ChannelOptions.allowRemoteHalfClosure, value: true)
    }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        let data = unwrapInboundIn(data)
        if case var .byteBuffer(bytes) = data.data { input.writeBuffer(&bytes) }
    }

    func userInboundEventTriggered(context: ChannelHandlerContext, event: Any) {
        if let exec = event as? SSHChannelRequestEvent.ExecRequest, exec.wantReply {
            if mode == .refuseExec {
                let channel = context.channel
                context.triggerUserOutboundEvent(ChannelFailureEvent()).whenComplete { _ in channel.close(promise: nil) }
            } else {
                context.triggerUserOutboundEvent(ChannelSuccessEvent(), promise: nil)
            }
            return
        }
        guard let event = event as? ChannelEvent, event == .inputClosed, mode != .hangExec else {
            context.fireUserInboundEventTriggered(event)
            return
        }
        let output = mode == .largeOutput ? ByteBuffer(repeating: 65, count: 270_000) : input
        context.channel.write(SSHChannelData(type: .channel, data: .byteBuffer(output)), promise: nil)
        context.channel.writeAndFlush(SSHChannelData(type: .stdErr, data: .byteBuffer(ByteBuffer(string: "fixture stderr"))), promise: nil)
        let channel = context.channel
        context.triggerUserOutboundEvent(SSHChannelRequestEvent.ExitStatus(exitStatus: 7)).whenComplete { _ in
            channel.close(promise: nil)
        }
    }
}
