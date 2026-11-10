import Foundation
#if canImport(Darwin)
    import Darwin
#elseif canImport(Glibc)
    import Glibc
#endif

/// The system-Tailscale guard shared by native discovery and SSH pairing.
/// WireGuard authenticates the hop only for destinations in the actual map:
///
/// - IPv4 in `100.64.0.0/10`, Tailscale's node range;
/// - IPv6 in `fd7a:115c:a1e0::/48`, Tailscale's ULA prefix;
/// - a MagicDNS machine name, `<machine>.<tailnet>.ts.net`.
///
/// `100.64.0.0/10` is also carrier-grade NAT space, and a `.ts.net` name is
/// answered by whatever DNS the phone uses, so the shape alone is not enough:
/// an unpinned SSH request is admitted only while a tunnel interface on this
/// phone holds a Tailscale address (``isInterfaceUp()``), and a name must
/// resolve to the netmap IPs immediately before each connection
/// (``resolve(_:)``). All connections dial the checked IP, not a second lookup.
enum SystemTailnet {
    static let magicDNSSuffix = ".ts.net"

    /// Whether a normalized (lowercased, unbracketed) host is a tailnet
    /// destination by shape.
    public static func isTailnetHost(_ host: String) -> Bool {
        if let bytes = ipv4(host) { return isTailnet(ipv4: bytes) }
        if let bytes = ipv6(host) { return isTailnet(ipv6: bytes) }
        return isMagicDNSName(host)
    }

    /// `<machine>.<tailnet>.ts.net` in letters, digits and hyphens: at least
    /// one label before the tailnet's, so a bare `ts.net` or `tailnet.ts.net`
    /// is not a machine.
    static func isMagicDNSName(_ host: String) -> Bool {
        guard host.hasSuffix(magicDNSSuffix) else { return false }
        let labels = host.split(separator: ".", omittingEmptySubsequences: false)
        return labels.count >= 4 && labels.allSatisfy(isLDHLabel)
    }

    private static func isLDHLabel(_ label: Substring) -> Bool {
        !label.isEmpty && label.utf8.count <= 63 && label.first != "-" && label.last != "-"
            && label.utf8.allSatisfy { byte in
            (UInt8(ascii: "a") ... UInt8(ascii: "z")).contains(byte)
                || (UInt8(ascii: "0") ... UInt8(ascii: "9")).contains(byte)
                || byte == UInt8(ascii: "-")
        }
    }

    /// Either Tailscale range, for an address in network byte order.
    static func isTailnet(address bytes: [UInt8]) -> Bool {
        isTailnet(ipv4: bytes) || isTailnet(ipv6: bytes)
    }

    static func isTailnet(ipv4 bytes: [UInt8]) -> Bool {
        bytes.count == 4 && bytes[0] == 100 && bytes[1] & 0xC0 == 64
    }

    static func isTailnet(ipv6 bytes: [UInt8]) -> Bool {
        bytes.count == 16 && Array(bytes.prefix(6)) == [0xFD, 0x7A, 0x11, 0x5C, 0xA1, 0xE0]
    }

    /// One interface address as `getifaddrs` reports it.
    struct InterfaceAddress: Equatable {
        let name: String
        let bytes: [UInt8]
    }

    /// Whether a tunnel (`utun*`) interface carries a Tailscale address. A
    /// carrier's CGNAT address sits on the cellular or Wi-Fi interface, never
    /// on a tunnel, so it does not count.
    static func carriesTailnetAddress(_ interfaces: [InterfaceAddress]) -> Bool {
        interfaces.contains { interface in
            interface.name.hasPrefix("utun")
                && isTailnet(address: interface.bytes)
        }
    }

    /// Whether Tailscale is up on this device right now.
    @Sendable public static func isInterfaceUp() -> Bool {
        carriesTailnetAddress(interfaceAddresses())
    }

    /// Every address the system resolver gives for `host`, in network byte
    /// order; empty when it fails. Runs off the cooperative pool, since
    /// `getaddrinfo` blocks.
    @Sendable public static func resolve(_ host: String) async -> [[UInt8]] {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .userInitiated).async {
                continuation.resume(returning: resolveBlocking(host))
            }
        }
    }

    private static func resolveBlocking(_ host: String) -> [[UInt8]] {
        var hints = addrinfo()
        hints.ai_family = AF_UNSPEC
        hints.ai_socktype = SOCK_STREAM
        var head: UnsafeMutablePointer<addrinfo>?
        guard getaddrinfo(host, nil, &hints, &head) == 0, let first = head else { return [] }
        defer { freeaddrinfo(head) }
        return sequence(first: first, next: { $0.pointee.ai_next }).compactMap { entry in
            entry.pointee.ai_addr.flatMap(addressBytes)
        }
    }

    static func interfaceAddresses() -> [InterfaceAddress] {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return [] }
        defer { freeifaddrs(head) }
        var result: [InterfaceAddress] = []
        for entry in sequence(first: first, next: { $0.pointee.ifa_next }) {
            guard let address = entry.pointee.ifa_addr,
                  let bytes = addressBytes(address)
            else { continue }
            let name = String(cString: entry.pointee.ifa_name)
            result.append(InterfaceAddress(name: name, bytes: bytes))
        }
        return result
    }

    private static func addressBytes(_ address: UnsafeMutablePointer<sockaddr>) -> [UInt8]? {
        switch Int32(address.pointee.sa_family) {
        case AF_INET:
            address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) {
                withUnsafeBytes(of: $0.pointee.sin_addr) { Array($0) }
            }
        case AF_INET6:
            address.withMemoryRebound(to: sockaddr_in6.self, capacity: 1) {
                withUnsafeBytes(of: $0.pointee.sin6_addr) { Array($0) }
            }
        default:
            nil
        }
    }

    static func ipv4(_ host: String) -> [UInt8]? {
        var address = in_addr()
        guard inet_pton(AF_INET, host, &address) == 1 else { return nil }
        return withUnsafeBytes(of: address) { Array($0) }
    }

    static func ipv6(_ host: String) -> [UInt8]? {
        var address = in6_addr()
        guard inet_pton(AF_INET6, host, &address) == 1 else { return nil }
        return withUnsafeBytes(of: address) { Array($0) }
    }
}
