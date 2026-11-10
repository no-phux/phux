import Foundation

/// Errors contain no remote output: pair JSON and stderr may contain credentials.
public enum TailnetPairingError: Error, Sendable, Equatable, LocalizedError {
    case systemTunnelRequired, otherTailnet, invalidPeer, invalidUsername
    case phuxNotInstalled, pairFailed, quicRequired, invalidPairing, destinationMismatch

    public var errorDescription: String? {
        switch self {
        case .systemTunnelRequired:
            "Turn on the Tailscale VPN on this phone, then try again."
        case .otherTailnet:
            "The phone's Tailscale VPN must use the same tailnet as the computer picker."
        case .invalidPeer:
            "Choose an online macOS or Linux computer with a tailnet address."
        case .invalidUsername:
            "Enter the computer's account username, without spaces or control characters."
        case .phuxNotInstalled:
            "Install phux on this computer first. This app does not install or change its server."
        case .pairFailed:
            "Pairing failed. Check that a current phux server is already running in paired mode with QUIC enabled."
        case .quicRequired:
            "This server needs a reachable QUIC listener. Enable QUIC on the computer, then try again."
        case .invalidPairing:
            "The computer returned an unsupported pairing document. Update phux on the computer."
        case .destinationMismatch:
            "The pairing destination does not match the computer selected from your tailnet."
        }
    }
}

public enum TailnetPairing {
    /// Discovery runs an embedded node, but connections use the installed
    /// system Tailscale VPN. A cellular CGNAT address is not sufficient.
    public static func hasSystemTunnel() -> Bool { SystemTailnet.isInterfaceUp() }

    /// Authenticate SSH, locate existing phux and mint enrollment material.
    /// The returned HTTPS link must pass the shared Rust pairing parser before
    /// it is persisted or dialed. Never log it or remote command output.
    public static func bootstrap(
        peer: TailnetPeer, username: String, password: String? = nil,
        onBanner: @escaping @Sendable (String) -> Void = { _ in }
    ) async throws -> String {
        let target = try await validatedTarget(
            peer: peer, username: username,
            tunnelUp: hasSystemTunnel, resolve: SystemTailnet.resolve
        )
        let policy: SSHHostKeyPolicy = peer.servesTailscaleSSH
            ? .pinned(peer.sshHostKeys) : .tailnetPeer
        let shell = try await SSHConnection.connect(
            to: target, password: password.flatMap { $0.isEmpty ? nil : $0 }, hostKeys: policy,
            onBanner: { banner in
                // Only an authenticated netmap-pinned Tailscale SSH check
                // URL reaches the UI, never arbitrary banner text/passwords.
                guard peer.servesTailscaleSSH,
                      let url = checkURL(in: banner, password: password)
                else { return }
                onBanner(url.absoluteString)
            }
        )
        do {
            let link = try await pair(on: shell, peer: peer)
            await shell.close()
            try Task.checkCancellation()
            return link
        } catch {
            await shell.close()
            throw error
        }
    }

    static func validatedTarget(
        peer: TailnetPeer, username: String,
        tunnelUp: @Sendable () -> Bool,
        resolve: @Sendable (String) async -> [[UInt8]]
    ) async throws -> SSHTarget {
        guard validUsername(username) else { throw TailnetPairingError.invalidUsername }
        let address = try checkedAddress(peer)
        guard tunnelUp() else { throw TailnetPairingError.systemTunnelRequired }
        if !peer.servesTailscaleSSH {
            guard await reachesSameTailnet(peer, resolve: resolve) else {
                throw TailnetPairingError.otherTailnet
            }
        }
        try Task.checkCancellation()
        guard tunnelUp() else { throw TailnetPairingError.systemTunnelRequired }
        // Dial the netmap IP, not a second DNS lookup after validation.
        return SSHTarget(host: address, username: username)
    }

    static func checkedAddress(_ peer: TailnetPeer) throws -> String {
        guard peer.isPickable, let address = peer.address,
              let bytes = addressBytes(address), SystemTailnet.isTailnet(address: bytes)
        else { throw TailnetPairingError.invalidPeer }
        return address
    }

    private static func reachesSameTailnet(
        _ peer: TailnetPeer, resolve: @Sendable (String) async -> [[UInt8]]
    ) async -> Bool {
        guard SystemTailnet.isMagicDNSName(peer.dnsName.lowercased()),
              let wanted = peer.address.flatMap(addressBytes) else { return false }
        let answers = await resolve(peer.dnsName)
        let known = peer.addresses.compactMap(addressBytes)
        return answers.contains(wanted) && answers.allSatisfy(known.contains)
    }

    static func validUsername(_ name: String) -> Bool {
        guard !name.isEmpty, name.utf8.count <= 256 else { return false }
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-@\\")
        return name.unicodeScalars.allSatisfy(allowed.contains)
    }

    static func checkURL(in banner: String, password: String?) -> URL? {
        guard banner.utf8.count <= 8192 else { return nil }
        return banner.split(whereSeparator: \.isWhitespace).lazy.compactMap { part in
            checkCandidate(String(part), password: password)
        }.first
    }

    private static func checkCandidate(_ value: String, password: String?) -> URL? {
        guard let parts = URLComponents(string: value), isCheckOrigin(parts),
              parts.fragment == nil, parts.query == nil, parts.path.hasPrefix("/a/")
        else { return nil }
        // A broken server must not echo a supplied password into the UI.
        if let password, !password.isEmpty,
           (value.removingPercentEncoding ?? value).contains(password) { return nil }
        return parts.url
    }

    private static func isCheckOrigin(_ parts: URLComponents) -> Bool {
        parts.scheme == "https" && parts.host == "login.tailscale.com"
            && parts.user == nil && parts.password == nil && parts.port == nil
    }

    private static func pair(on shell: SSHConnection, peer: TailnetPeer) async throws -> String {
        try Task.checkCancellation()
        let found = try await shell.run("sh -s", stdin: PairingScripts.locate, timeout: .seconds(15))
        guard found.succeeded else { throw TailnetPairingError.phuxNotInstalled }
        guard let path = PairingScripts.answer(in: found.stdout)?.trimmingCharacters(in: .newlines),
              path.hasPrefix("/"), path.utf8.count <= 4096,
              !path.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains)
        else { throw TailnetPairingError.phuxNotInstalled }
        try Task.checkCancellation()
        let paired = try await shell.run(
            "sh -s", stdin: PairingScripts.pair(phux: path, name: peer.name), timeout: .seconds(30)
        )
        guard paired.succeeded else { throw TailnetPairingError.pairFailed }
        guard let answer = PairingScripts.answer(in: paired.stdout) else {
            throw TailnetPairingError.invalidPairing
        }
        return try PairingDocument.link(json: Data(answer.utf8), peer: peer)
    }

    static func addressBytes(_ host: String) -> [UInt8]? {
        SystemTailnet.ipv4(host) ?? SystemTailnet.ipv6(host)
    }
}

enum PairingScripts {
    static let marker = "@@rune-pairing-v1@@"

    // Noninteractive SSH often omits the user's package-manager PATH.
    // PHUX_LOCATE_ROOT is used only by disposable shell tests.
    static let locate = """
    umask 077
    root="${PHUX_LOCATE_ROOT-}"
    for p in "$(command -v phux 2>/dev/null)" "$HOME/.local/bin/phux" \
      "$root/opt/homebrew/bin/phux" "$root/usr/local/bin/phux" "$HOME/.cargo/bin/phux" \
      "$HOME/.nix-profile/bin/phux"; do
      if [ -n "$p" ] && [ -f "$p" ] && [ -x "$p" ]; then
        printf '%s\\n%s\\n' '\(marker)' "$p"
        exit 0
      fi
    done
    exit 127
    """

    static func pair(phux: String, name: String) -> String {
        "umask 077\nprintf '%s\\n' '\(marker)'\n"
            + "exec \(quote(phux)) pair --json --enroll --name \(quote(name))\n"
    }

    static func quote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    static func answer(in stdout: String) -> String? {
        guard stdout.utf8.count <= 262_144 else { return nil }
        let lines = stdout.split(separator: "\n", omittingEmptySubsequences: false)
        guard let last = lines.lastIndex(of: Substring(marker)) else { return nil }
        return lines[(last + 1)...].joined(separator: "\n")
    }
}
