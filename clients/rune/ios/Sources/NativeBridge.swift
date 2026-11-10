import Foundation
import UIKit

// The wake context is independently retained by Rust's last Arc<Waker>.
// Its weak UIKit owner is read only after dispatching to the main queue.
final class WakeBox: @unchecked Sendable {
    weak var view: RuneView?
    func schedule() { DispatchQueue.main.async { [self] in view?.wake() } }
}

private let nativeWake: @convention(c) (UnsafeMutableRawPointer?) -> Void = { context in
    guard let context else { return }
    Unmanaged<WakeBox>.fromOpaque(context).takeUnretainedValue().schedule()
}
private let nativeRelease: @convention(c) (UnsafeMutableRawPointer?) -> Void = { context in
    if let context { Unmanaged<WakeBox>.fromOpaque(context).release() }
}

struct InputContext {
    let token: UInt64
    let revision: UInt64
    let kind: String
    let text: String
    let anchor: Int
    let focus: Int
    let multiline: Bool
    let optionAsMeta: Bool
    let wanted: Bool
    let caret: CGRect?

    init?(_ value: Any?) {
        guard let value = value as? [String: Any], let token = value["token"] as? UInt64,
              let revision = value["revision"] as? UInt64, let kind = value["kind"] as? String,
              let text = value["text"] as? String, let anchor = value["anchor"] as? Int,
              let focus = value["focus"] as? Int else { return nil }
        self.token = token; self.revision = revision; self.kind = kind; self.text = text
        self.anchor = anchor; self.focus = focus
        multiline = value["multiline"] as? Bool ?? false
        optionAsMeta = value["option_as_meta"] as? Bool ?? false
        wanted = value["wanted"] as? Bool ?? false
        if let r = value["caret"] as? [Double], r.count == 4 {
            caret = CGRect(x: r[0], y: r[1], width: r[2], height: r[3])
        } else { caret = nil }
    }
}

// NSRange loses the active end of a selection. Keep Rune's directional pair
// alongside UIKit's normalized range, including unchanged selection callbacks.
struct InputSelection {
    var anchor: Int
    var focus: Int
    var range: NSRange {
        NSRange(location: min(anchor, focus), length: abs(anchor - focus))
    }

    mutating func update(_ range: NSRange) {
        guard range != self.range else { return }
        let start = range.location, end = NSMaxRange(range)
        if start == anchor { focus = end }
        else if end == anchor { focus = start }
        else if anchor > focus { anchor = end; focus = start }
        else { anchor = start; focus = end }
    }
}

@MainActor
final class NativeBridge {
    private var pointer: UnsafeMutableRawPointer?
    private let identity: NativeIdentity?
    isolated deinit { close() }

    init(layer: CAMetalLayer, size: CGSize, scale: CGFloat, dark: Bool,
         configuration: [String: Any], wake: WakeBox) throws {
        identity = configuration["chain_pem"] == nil ? nil : try NativeIdentity(configuration:configuration)
        let bytes = try JSONSerialization.data(withJSONObject: configuration)
        let text = String(decoding: bytes, as: UTF8.self)
        let context = Unmanaged.passRetained(wake).toOpaque()
        // Ownership of the retained wake box transfers even when create fails.
        pointer = text.withCString {
            phux_rune_ios_create(Unmanaged.passUnretained(layer).toOpaque(), size.width,
                                 size.height, scale, dark, $0, context, nativeWake, nativeRelease, identity?.handle)
        }
        if pointer == nil { throw NativeFailure.message(Self.error()) }
    }

    func close() {
        guard let pointer else { return }
        self.pointer = nil
        phux_rune_ios_destroy(pointer)
    }

    static func validate(_ configuration: [String: Any]) throws {
        let bytes = try JSONSerialization.data(withJSONObject:configuration)
        _ = try String(decoding:bytes,as:UTF8.self).withCString { try object(phux_rune_ios_validate($0)) }
    }

    func resize(_ size: CGSize, scale: CGFloat) {
        phux_rune_ios_resize(pointer, size.width, size.height, scale)
    }

    func frame() throws -> [String: Any] { try Self.object(phux_rune_ios_frame(pointer)) }
    func report(marker: String) throws -> [String: Any] {
        try marker.withCString { try Self.object(phux_rune_ios_report(pointer, $0)) }
    }
    func tree() throws -> [String: Any] { try Self.object(phux_rune_ios_tree(pointer)) }
    func input(_ operation: [String: Any], context: InputContext) throws -> [String: Any] {
        var operation = operation
        operation["token"] = context.token
        operation["revision"] = context.revision
        let data = try JSONSerialization.data(withJSONObject: operation)
        return try String(decoding: data, as: UTF8.self).withCString {
            try Self.object(phux_rune_ios_input(pointer, $0))
        }
    }
    func touch(_ identity: UInt64, phase: UInt32, point: CGPoint, seconds: Double) {
        phux_rune_ios_touch(pointer, identity, phase, point.x, point.y, seconds)
    }
    func paste(_ text: String) { text.withCString { phux_rune_ios_paste(pointer, $0) } }
    func active(_ active: Bool) { phux_rune_ios_active(pointer, active) }
    func traits(dark: Bool, reducedMotion: Bool) { phux_rune_ios_traits(pointer, dark, reducedMotion) }

    static func object(_ pointer: UnsafeMutablePointer<CChar>?) throws -> [String: Any] {
        guard let pointer else { throw NativeFailure.message(error()) }
        defer { phux_rune_ios_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw NativeFailure.message("Rune returned invalid native state.")
        }
        return object
    }
    private static func error() -> String {
        guard let pointer = phux_rune_ios_error() else { return "Rune could not start." }
        defer { phux_rune_ios_free(pointer) }
        let data = Data(String(cString: pointer).utf8)
        let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return object?["error"] as? String ?? "Rune could not start."
    }
}

enum NativeFailure: LocalizedError {
    case message(String)
    var errorDescription: String? { if case let .message(message) = self { message } else { nil } }
}
