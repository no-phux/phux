import Foundation

/// This layer constrains destinations to the authenticated SSH peer. Rust owns
/// credential, certificate, CA and enrollment trust validation for all inputs.
struct PairingDocument: Decodable {
    var schema_version: Int
    var token: String
    var cert_fingerprint: String?
    var ca_fingerprint: String?
    var enrollment_ticket: String?
    var quic_addr: String?
    var connect_link: String?

    static func link(json: Data, peer: TailnetPeer) throws -> String {
        guard json.count <= 262_144,
              let document = try? JSONDecoder().decode(Self.self, from: json),
              document.schema_version == 1, !document.token.isEmpty
        else { throw TailnetPairingError.invalidPairing }
        return try document.link(peer: peer)
    }

    private func link(peer: TailnetPeer) throws -> String {
        var fields = try Self.fields(connect_link)
        if let ws = fields["url"] { _ = try Self.endpoint(ws, scheme: "wss", peer: peer) }
        let quic = fields["quic"] ?? quic_addr.map { "quic://\($0)" }
        guard let quic else { throw TailnetPairingError.quicRequired }
        let endpoint = try Self.endpoint(quic, scheme: "quic", peer: peer)
        fields["quic"] = endpoint
        fields["url"] = nil // Rune dials QUIC; never retain a second destination.
        fields["token"] = fields["token"] ?? token
        fields["fp"] = fields["fp"] ?? cert_fingerprint
        fields["ca"] = fields["ca"] ?? ca_fingerprint
        fields["enroll"] = fields["enroll"] ?? enrollment_ticket
        fields["name"] = fields["name"] ?? peer.name
        guard fields["enroll"]?.isEmpty == false else { throw TailnetPairingError.invalidPairing }
        var result = URLComponents()
        result.scheme = "https"
        result.host = "phux.sh"
        result.path = "/connect"
        result.queryItems = ["quic", "name", "fp", "ca", "enroll", "token"].compactMap { key in
            fields[key].map { URLQueryItem(name: key, value: $0) }
        }
        guard let link = result.string else { throw TailnetPairingError.invalidPairing }
        return link
    }

    private static func fields(_ link: String?) throws -> [String: String] {
        guard let link else { return [:] }
        // `phux pair` puts the token and ticket after `#`; older hosts put them
        // in the query. One field list, so a field named in both is refused.
        guard let parts = canonicalConnectURL(link),
              case let items = (parts.queryItems ?? []) + (parts.percentEncodedFragment
                  .flatMap { URLComponents(string: "?" + $0)?.queryItems } ?? []),
              !items.isEmpty
        else { throw TailnetPairingError.invalidPairing }
        let allowed: Set<String> = ["url", "quic", "name", "fp", "ca", "enroll", "token"]
        var result: [String: String] = [:]
        for item in items {
            guard allowed.contains(item.name), result[item.name] == nil,
                  let value = item.value, !value.isEmpty
            else { throw TailnetPairingError.invalidPairing }
            result[item.name] = value
        }
        return result
    }

    private static func canonicalConnectURL(_ link: String) -> URLComponents? {
        guard link.utf8.count <= 65_536, let parts = URLComponents(string: link) else { return nil }
        guard parts.scheme == "https", parts.host == "phux.sh", parts.path == "/connect",
              parts.user == nil, parts.password == nil, parts.port == nil
        else { return nil }
        return parts
    }

    private static func endpoint(_ value: String, scheme: String, peer: TailnetPeer) throws -> String {
        guard let parts = URLComponents(string: value), parts.scheme == scheme, isBareEndpoint(parts),
              let host = parts.host?.trimmingCharacters(in: CharacterSet(charactersIn: "[]")),
              let port = parts.port, (1...65535).contains(port)
        else { throw TailnetPairingError.quicRequired }
        let address = try TailnetPairing.checkedAddress(peer)
        guard matchesPeer(host, peer: peer) else {
            throw TailnetPairingError.destinationMismatch
        }
        // Wildcard listeners, and exact netmap DNS names, are reached using
        // the IP already authenticated by SSH. No DNS credential redirection.
        let authority = address.contains(":") ? "[\(address)]" : address
        return "\(scheme)://\(authority):\(port)"
    }

    private static func isBareEndpoint(_ parts: URLComponents) -> Bool {
        parts.user == nil && parts.password == nil && parts.query == nil
            && parts.fragment == nil && parts.path.isEmpty
    }

    private static func matchesPeer(_ host: String, peer: TailnetPeer) -> Bool {
        if host == "0.0.0.0" || host == "::" { return true }
        if let bytes = TailnetPairing.addressBytes(host) {
            return peer.addresses.compactMap(TailnetPairing.addressBytes).contains(bytes)
        }
        return !peer.dnsName.isEmpty && host.lowercased() == peer.dnsName.lowercased()
    }
}
