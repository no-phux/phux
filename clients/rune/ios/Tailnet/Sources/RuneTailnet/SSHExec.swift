import NIOCore
import NIOSSH

// The exec half of the SSH client (SSHClient.swift): one command on one
// session channel.

/// One exec request: collects stdout, stderr, and the exit status, and
/// completes when the channel closes.
final class ExecHandler: ChannelDuplexHandler, @unchecked Sendable {
    typealias InboundIn = SSHChannelData
    typealias OutboundIn = SSHChannelData
    typealias OutboundOut = SSHChannelData

    private let command: String
    private let stdin: String?
    private let result: EventLoopPromise<SSHCommandResult>
    private var stdout = ByteBuffer()
    private var stderr = ByteBuffer()
    private var exitStatus: Int?
    /// The remote signal that ended the command, if one did.
    private var exitSignal: String?

    init(command: String, stdin: String?, result: EventLoopPromise<SSHCommandResult>) {
        self.command = command
        self.stdin = stdin
        self.result = result
    }

    func handlerAdded(context: ChannelHandlerContext) {
        _ = context.channel.setOption(ChannelOptions.allowRemoteHalfClosure, value: true)
    }

    func channelActive(context: ChannelHandlerContext) {
        let request = SSHChannelRequestEvent.ExecRequest(command: command, wantReply: true)
        let bound = NIOLoopBound(context, eventLoop: context.eventLoop)
        context.triggerUserOutboundEvent(request).whenComplete { [stdin, result] outcome in
            let context = bound.value
            guard case .success = outcome else {
                result.fail(SSHFailure.commandRefused)
                context.close(promise: nil)
                return
            }
            if let stdin {
                let bytes = context.channel.allocator.buffer(string: stdin)
                context.write(
                    NIOAny(SSHChannelData(type: .channel, data: .byteBuffer(bytes))), promise: nil
                )
            }
            context.flush()
            context.close(mode: .output, promise: nil)
        }
        context.fireChannelActive()
    }

    func channelRead(context: ChannelHandlerContext, data: NIOAny) {
        let message = unwrapInboundIn(data)
        guard case var .byteBuffer(bytes) = message.data else { return }
        guard stdout.readableBytes + stderr.readableBytes + bytes.readableBytes <= 262_144 else {
            result.fail(SSHFailure.outputTooLarge)
            context.close(promise: nil)
            return
        }
        switch message.type {
        case .channel: stdout.writeBuffer(&bytes)
        case .stdErr: stderr.writeBuffer(&bytes)
        default: break
        }
    }

    func userInboundEventTriggered(context: ChannelHandlerContext, event: Any) {
        switch event {
        case let status as SSHChannelRequestEvent.ExitStatus:
            exitStatus = status.exitStatus
        case let signal as SSHChannelRequestEvent.ExitSignal:
            exitSignal = signal.signalName
        case is ChannelFailureEvent:
            result.fail(SSHFailure.commandRefused)
            context.close(promise: nil)
        default:
            context.fireUserInboundEventTriggered(event)
        }
    }

    func channelInactive(context: ChannelHandlerContext) {
        if exitStatus == nil, exitSignal == nil {
            // The connection dropped mid-command: nothing finished.
            result.fail(SSHFailure.commandRefused)
        } else {
            var errors = String(buffer: stderr)
            if let exitSignal { errors += "killed by signal \(exitSignal)\n" }
            result.succeed(SSHCommandResult(
                exitStatus: exitStatus, stdout: String(buffer: stdout), stderr: errors
            ))
        }
        context.fireChannelInactive()
    }

    func errorCaught(context: ChannelHandlerContext, error _: any Error) {
        result.fail(SSHFailure.commandRefused)
        context.close(promise: nil)
    }
}
