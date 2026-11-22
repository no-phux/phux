import Foundation

// The slice of Tailscale's `ipnstate.Status` document (LocalAPI `/status`)
// the onboarding picker reads: whether the embedded node is signed in, the
// URL that signs it in, and the person's other machines. Decoded leniently —
// every field optional — because the document is Tailscale's, not ours, and
// grows between releases.

/// One machine on the tailnet, as the picker shows it.
public struct TailnetPeer: Sendable, Equatable, Identifiable {
    public enum Platform: Sendable, Equatable {
        case macOS, linux
        case other(String)
    }

    public var id: String
    /// The machine name (`studio`), the first label of its MagicDNS name.
    public var name: String
    /// The full MagicDNS name, without the trailing dot; empty when unknown.
    public var dnsName: String
    public var platform: Platform
    public var online: Bool
    /// The node's tailnet IPv4 address, which the SSH dial uses.
    public var address: String?
    /// The OpenSSH host keys tailnet control reports for the node: present
    /// when it serves Tailscale SSH.
    public var sshHostKeys: [String]
    /// The login that owns the node (`me@example.com`); nil for tagged nodes.
    public var owner: String?
    /// All authenticated netmap IPs, including IPv6. Kept internal so a
    /// dual-stack DNS response can be compared with the complete node map.
    var addresses: [String]

    public init(
        id: String, name: String, dnsName: String, platform: Platform, online: Bool,
        address: String?, sshHostKeys: [String] = [], owner: String? = nil
    ) {
        self.id = id
        self.name = name
        self.dnsName = dnsName
        self.platform = platform
        self.online = online
        self.address = address
        self.sshHostKeys = sshHostKeys
        self.owner = owner
        self.addresses = address.map { [$0] } ?? []
    }

    /// phux runs on macOS and Linux; everything else is listed, not offered.
    public var canRunPhux: Bool { platform == .macOS || platform == .linux }

    /// A Tailscale SSH node admits the tailnet identity with no password.
    public var servesTailscaleSSH: Bool { !sshHostKeys.isEmpty }

    /// Whether the picker offers this machine: phux runs there, it is up,
    /// and it has an address to dial.
    public var isPickable: Bool { canRunPhux && online && address != nil }

}

/// What the embedded node reports.
public struct TailnetSnapshot: Sendable, Equatable {
    public enum Phase: Sendable, Equatable {
        /// Starting up, or between states.
        case starting
        /// Not signed in; `loginURL` signs this node in once control offers it.
        case needsLogin(loginURL: URL?)
        /// Signed in, waiting for the tailnet's admin to approve the device.
        case needsApproval
        /// Signed in and connected.
        case running
        /// Stopped, or a state this build does not know.
        case stopped(String)
    }

    public var phase: Phase
    /// The tailnet's display name (`me@example.com`, `example.org`).
    public var tailnetName: String?
    /// Peers, pickable first, then online, then by name.
    public var peers: [TailnetPeer]

    public init(phase: Phase, tailnetName: String? = nil, peers: [TailnetPeer] = []) {
        self.phase = phase
        self.tailnetName = tailnetName
        self.peers = peers
    }

    /// Decode a LocalAPI `/status` document.
    public static func decode(_ data: Data) throws -> TailnetSnapshot {
        let status = try JSONDecoder().decode(StatusDocument.self, from: data)
        return TailnetSnapshot(status)
    }

    init(_ status: StatusDocument) {
        phase = Self.phase(status.BackendState, authURL: status.AuthURL)
        tailnetName = status.CurrentTailnet?.Name
        let logins = (status.User ?? [:]).reduce(into: [String: String]()) { names, entry in
            if let login = entry.value.LoginName { names[entry.key] = login }
        }
        peers = (status.Peer ?? [:]).map { key, peer in Self.peer(peer, key: key, logins: logins) }
            .sorted(by: Self.pickerOrder)
    }

    private static func phase(_ backend: String?, authURL: String?) -> Phase {
        switch backend {
        case "Running": .running
        case "NeedsLogin":
            .needsLogin(loginURL: authURL.flatMap { $0.isEmpty ? nil : URL(string: $0) })
        case "NeedsMachineAuth": .needsApproval
        case nil, "", "NoState", "Starting": .starting
        case let other?: .stopped(other)
        }
    }

    private static func peer(
        _ peer: PeerDocument, key: String, logins: [String: String]
    ) -> TailnetPeer {
        let dnsName = (peer.DNSName ?? "").trimmingCharacters(in: CharacterSet(charactersIn: "."))
        let label = dnsName.split(separator: ".").first.map(String.init)
        let host = peer.HostName.flatMap { $0.isEmpty || $0 == "localhost" ? nil : $0 }
        let name = label ?? host ?? key
        let addresses = peer.TailscaleIPs ?? []
        let address = addresses.first(where: isTailnetIPv4)
            ?? addresses.first { SystemTailnet.ipv6($0).map(SystemTailnet.isTailnet(ipv6:)) == true }
        let owner = peer.UserID.flatMap { logins[String($0)] }
        // A tagged node belongs to no person; its "user" is the tagger.
        let isTagged = !(peer.Tags ?? []).isEmpty
        var result = TailnetPeer(
            id: peer.ID ?? key,
            name: name,
            dnsName: dnsName,
            platform: platform(peer.OS),
            online: peer.Online ?? false,
            address: address,
            sshHostKeys: peer.sshHostKeys ?? [],
            owner: isTagged ? nil : owner
        )
        result.addresses = addresses.filter {
            TailnetPairing.addressBytes($0).map(SystemTailnet.isTailnet(address:)) == true
        }
        return result
    }

    static func platform(_ os: String?) -> TailnetPeer.Platform {
        switch os?.lowercased() {
        case "macos": .macOS
        case "linux": .linux
        default: .other(os ?? "")
        }
    }

    static func isTailnetIPv4(_ address: String) -> Bool {
        SystemTailnet.ipv4(address).map(SystemTailnet.isTailnet(ipv4:)) == true
    }

    static func pickerOrder(_ lhs: TailnetPeer, _ rhs: TailnetPeer) -> Bool {
        if lhs.isPickable != rhs.isPickable { return lhs.isPickable }
        if lhs.canRunPhux != rhs.canRunPhux { return lhs.canRunPhux }
        if lhs.online != rhs.online { return lhs.online }
        return lhs.name.localizedStandardCompare(rhs.name) == .orderedAscending
    }
}

// Field names are Tailscale's Go spellings.
// swiftlint:disable identifier_name
struct StatusDocument: Decodable {
    var BackendState: String?
    var AuthURL: String?
    var CurrentTailnet: TailnetDocument?
    var Peer: [String: PeerDocument]?
    var User: [String: UserDocument]?
}

struct TailnetDocument: Decodable {
    var Name: String?
}

struct PeerDocument: Decodable {
    var ID: String?
    var HostName: String?
    var DNSName: String?
    var OS: String?
    var UserID: Int64?
    var TailscaleIPs: [String]?
    var Online: Bool?
    var Tags: [String]?
    var sshHostKeys: [String]?
}

struct UserDocument: Decodable {
    var LoginName: String?
}

// swiftlint:enable identifier_name
