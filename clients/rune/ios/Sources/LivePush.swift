import ActivityKit
import Foundation

/// Keeps the Lock Screen card current while the app is closed: the phone's
/// computers post their agent counts to the gateway (build.py
/// `--live-gateway`), which pushes them to the card. No account: the grant
/// minted here is the only key, and the gateway stores only its hash.
@MainActor
enum LivePush {
    /// The gateway's functions URL; nil in a build without one, which keeps
    /// the card local.
    nonisolated static let gateway = Bundle.main.object(forInfoDictionaryKey: "PhuxLiveGateway") as? String

    /// What Rune writes to each computer: this phone's id and its grant.
    static var device: String { stored("phux.live.device") { UUID().uuidString.lowercased() } }
    static var grant: String {
        stored("phux.live.grant") {
            var bytes = [UInt8](repeating: 0, count: 32)
            _ = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
            return Data(bytes).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_")
                .replacingOccurrences(of: "=", with: "")
        }
    }

    private static var listening = false

    /// From launch: the token that lets a computer start the card, and each
    /// card's own token, including a card a push started while closed.
    static func listen() {
        guard gateway != nil, !listening else { return }
        listening = true
        Task {
            for await token in Activity<AgentActivity>.pushToStartTokenUpdates {
                register(["start_token": hex(token)])
            }
        }
        Task {
            for await activity in Activity<AgentActivity>.activityUpdates { follow(activity) }
        }
        for activity in Activity<AgentActivity>.activities { follow(activity) }
    }

    static func follow(_ activity: Activity<AgentActivity>) {
        Task {
            for await token in activity.pushTokenUpdates { register(["activity_token": hex(token)]) }
        }
    }

    private static func register(_ fields: [String: String]) {
        guard let gateway, let url = URL(string: gateway + "/live-register") else { return }
        var body: [String: Any] = fields
        body["grant"] = grant
        body["environment"] = environment
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: body)
        // ponytail: one try; a lost registration waits for the next token.
        Task.detached {
            do {
                let (_, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status != 200 { NSLog("The Lock Screen card's gateway answered %d.", status) }
            } catch { NSLog("The Lock Screen card's gateway did not answer: %@", error.localizedDescription) }
        }
    }

    /// Which APNs a token belongs to: an App Store or TestFlight build has no
    /// embedded profile and is production; a development build has one.
    private static var environment: String {
        #if targetEnvironment(simulator)
        "development"
        #else
        Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision") == nil
            ? "production" : "development"
        #endif
    }

    private static func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    private static func stored(_ key: String, _ make: () -> String) -> String {
        if let value = UserDefaults.standard.string(forKey: key) { return value }
        let value = make()
        UserDefaults.standard.set(value, forKey: key)
        return value
    }
}
