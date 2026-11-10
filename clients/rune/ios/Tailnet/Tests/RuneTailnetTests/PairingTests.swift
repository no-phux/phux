import Foundation
import XCTest
@testable import RuneTailnet

@MainActor
final class PairingTests: XCTestCase {
    private var peer: TailnetPeer {
        TailnetPeer(id: "node", name: "mini", dnsName: "mini.example.ts.net", platform: .macOS,
                    online: true, address: "100.64.1.2")
    }

    func testSnapshotsDecodeNetmap() throws {
        let json = Data("""
        {"BackendState":"Running","Peer":{"node":{"DNSName":"mini.example.ts.net.",
        "OS":"macos","Online":true,"TailscaleIPs":["192.168.1.2","100.64.1.2"],
        "sshHostKeys":["ssh-ed25519 AAAA"],"UserID":1}},"User":{"1":{"LoginName":"me@example.com"}}}
        """.utf8)
        let snapshot = try TailnetSnapshot.decode(json)
        XCTAssertEqual(snapshot.phase, .running)
        let machine = try XCTUnwrap(snapshot.peers.first)
        XCTAssertEqual(machine.address, peer.address)
        XCTAssertEqual(machine.dnsName, peer.dnsName)
        XCTAssertEqual(machine.suggestedUsername, "me")
        XCTAssertTrue(machine.isPickable)
        XCTAssertTrue(machine.servesTailscaleSSH)
        XCTAssertFalse(TailnetSnapshot.isTailnetIPv4("100.64.bad.1.2"))
        XCTAssertFalse(TailnetSnapshot.isTailnetIPv4("100.128.1.2"))
    }

    func testSystemTunnelExcludesCarrierCGNATAndForeignIPv6() {
        XCTAssertFalse(SystemTailnet.carriesTailnetAddress([.init(name: "en0", bytes: [100, 64, 1, 2])]))
        XCTAssertTrue(SystemTailnet.carriesTailnetAddress([.init(name: "utun8", bytes: [100, 64, 1, 2])]))
        XCTAssertFalse(SystemTailnet.carriesTailnetAddress([.init(name: "utun8", bytes: [100, 128, 1, 2])]))
        let ipv6 = SystemTailnet.ipv6("fd7a:115c:a1e0::1")!
        XCTAssertTrue(SystemTailnet.carriesTailnetAddress([.init(name: "utun1", bytes: ipv6)]))
        XCTAssertFalse(SystemTailnet.isTailnet(ipv6: SystemTailnet.ipv6("fd7a:115c:a1e1::1")!))
    }

    func testOrdinarySSHRequiresSameTailnetAndSystemTunnel() async throws {
        let target = try await TailnetPairing.validatedTarget(
            peer: peer, username: "me", tunnelUp: { true }, resolve: { _ in [[100, 64, 1, 2]] }
        )
        XCTAssertEqual(target.host, peer.address)
        let invalidAnswers: [[[UInt8]]] = [[], [[100, 64, 1, 3]], [[100, 64, 1, 2], [8, 8, 8, 8]]]
        for answers in invalidAnswers {
            do {
                _ = try await TailnetPairing.validatedTarget(
                    peer: peer, username: "me", tunnelUp: { true }, resolve: { _ in answers }
                )
                XCTFail("accepted a different tailnet")
            } catch { XCTAssertEqual(error as? TailnetPairingError, .otherTailnet) }
        }
        do {
            _ = try await TailnetPairing.validatedTarget(
                peer: peer, username: "me", tunnelUp: { false }, resolve: { _ in [[100, 64, 1, 2]] }
            )
            XCTFail("accepted missing VPN")
        } catch { XCTAssertEqual(error as? TailnetPairingError, .systemTunnelRequired) }
    }

    func testOrdinarySSHPreservesDualStackNetmapAgreement() async throws {
        var dualStack = peer
        dualStack.addresses = [peer.address!, "fd7a:115c:a1e0::2"]
        let answers = dualStack.addresses.compactMap(TailnetPairing.addressBytes)
        _ = try await TailnetPairing.validatedTarget(
            peer: dualStack, username: "me", tunnelUp: { true }, resolve: { _ in answers }
        )
    }

    func testPinnedSSHDoesNotUseDNSButStillRequiresTunnel() async throws {
        var pinned = peer
        pinned.sshHostKeys = ["ssh-ed25519 AAAA"]
        _ = try await TailnetPairing.validatedTarget(
            peer: pinned, username: "me", tunnelUp: { true }, resolve: { _ in [] }
        )
        XCTAssertTrue(SSHHostKeyPolicyCheck.accepts("ssh-ed25519 AAAA comment", policy: .pinned(pinned.sshHostKeys)))
        XCTAssertFalse(SSHHostKeyPolicyCheck.accepts("ssh-ed25519 BBBB", policy: .pinned(pinned.sshHostKeys)))
        XCTAssertFalse(SSHHostKeyPolicyCheck.accepts("", policy: .pinned([])))
    }

    func testUsernameAndBannerAreNotShellOrCredentialChannels() {
        for name in ["", " ", "me\n", "$(id)", "me;echo", "me name", "me\u{1b}"] {
            XCTAssertFalse(TailnetPairing.validUsername(name))
        }
        XCTAssertTrue(TailnetPairing.validUsername("my-user_1"))
        XCTAssertEqual(TailnetPairing.checkURL(in: "Check https://login.tailscale.com/a/123", password: nil)?.host,
                       "login.tailscale.com")
        for banner in ["password is secret", "https://evil.test/a/123", "https://login.tailscale.com/a/secret",
                       "https://user@login.tailscale.com/a/123", "https://login.tailscale.com/a/123?token=secret"] {
            XCTAssertNil(TailnetPairing.checkURL(in: banner, password: "secret"))
        }
    }

    func document(_ additions: [String: Any] = [:]) throws -> Data {
        var object: [String: Any] = ["schema_version": 1, "token": "secret-token", "cert_fingerprint": "leaf",
                                     "ca_fingerprint": "ca", "enrollment_ticket": "enrollment",
                                     "quic_addr": "0.0.0.0:8788"]
        object.merge(additions) { _, new in new }
        return try JSONSerialization.data(withJSONObject: object)
    }

    func testSynthesizesDirectCanonicalLinkForWildcardQUIC() throws {
        let link = try PairingDocument.link(json: document(), peer: peer)
        let parts = try XCTUnwrap(URLComponents(string: link))
        XCTAssertEqual(parts.scheme, "https")
        XCTAssertEqual(parts.host, "phux.sh")
        XCTAssertEqual(parts.path, "/connect")
        let fields = Dictionary(uniqueKeysWithValues: parts.queryItems!.map { ($0.name, $0.value!) })
        XCTAssertEqual(fields["quic"], "quic://100.64.1.2:8788")
        XCTAssertEqual(fields["token"], "secret-token")
        XCTAssertEqual(fields["enroll"], "enrollment")
        XCTAssertNil(fields["url"])
    }

    func testServerLinkRetainsPinsButCannotRedirectCredentials() throws {
        let good = "https://phux.sh/connect?url=wss://mini.example.ts.net:8787&quic=quic://100.64.1.2:8788&fp=from-link&ca=ca&enroll=ticket&token=token"
        let link = try PairingDocument.link(json: document(["connect_link": good]), peer: peer)
        XCTAssertTrue(link.contains("fp=from-link"))
        XCTAssertFalse(link.contains("wss"))
        for bad in [
            good.replacingOccurrences(of: "mini.example.ts.net", with: "attacker.test"),
            good.replacingOccurrences(of: "100.64.1.2", with: "100.64.1.3"),
            good.replacingOccurrences(of: "https://phux.sh", with: "https://attacker.test"),
            good + "&token=duplicate", good + "#fragment", good + "&sni=relay"
        ] {
            XCTAssertThrowsError(try PairingDocument.link(json: document(["connect_link": bad]), peer: peer))
        }
    }

    func testRefusesMissingQUICAndLoopback() throws {
        for bind in ["127.0.0.1:8788", "[::1]:8788", "0.0.0.0:0", "0.0.0.0:65536", "evil.test:8788"] {
            XCTAssertThrowsError(try PairingDocument.link(json: document(["quic_addr": bind]), peer: peer))
        }
        XCTAssertThrowsError(try PairingDocument.link(json: document(["quic_addr": NSNull()]), peer: peer))
        XCTAssertThrowsError(try PairingDocument.link(json: document(["schema_version": 2]), peer: peer))
    }

    func testMarkerAndShellQuoting() {
        XCTAssertEqual(PairingScripts.answer(in: "chatter\n\(PairingScripts.marker)\nold\n\(PairingScripts.marker)\n{}\n"), "{}\n")
        XCTAssertNil(PairingScripts.answer(in: "{}"))
        XCTAssertNil(PairingScripts.answer(in: String(repeating: "x", count: 262_145)))
        XCTAssertEqual(PairingScripts.quote("a'b"), "'a'\\''b'")
        let pair = PairingScripts.pair(phux: "/path with spaces/phux", name: "My 'mini'")
        XCTAssertTrue(pair.contains("umask 077"))
        XCTAssertTrue(pair.contains("pair --json --enroll --name"))
        for forbidden in ["install", "upgrade", "--ensure", "--adopt", "curl", "wget"] {
            XCTAssertFalse(pair.contains(forbidden))
        }
    }
}
