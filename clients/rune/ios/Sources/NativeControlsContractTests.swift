import UIKit

#if targetEnvironment(simulator)

/// Private UIKit views only: never mounts into Rune's window or takes input.
@MainActor
enum NativeControlsContractTests {
    private struct Failure: Error, CustomStringConvertible {
        let description: String
    }

    @MainActor
    private final class CountingHost: UIView {
        var reorders = 0
        override func bringSubviewToFront(_ view: UIView) {
            reorders += 1
            super.bringSubviewToFront(view)
        }
    }

    private static func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
        if !condition() { throw Failure(description: message) }
    }

    private static func specs(_ ids: [String]) -> [[String: Any]] {
        ids.map { id in
            ["id": id, "label": id, "symbol": "circle", "title": id.uppercased(),
             "rect": [20.0, 40.0, 100.0, 48.0], "ink": [1.0, 1.0, 1.0],
             "enabled": true, "dark": true, "opacity": 1.0]
        }
    }

    private static func requireOrder(_ host: CountingHost, _ ids: [String]) throws {
        let suffix = host.subviews.suffix(ids.count).compactMap(\.accessibilityLabel)
        try require(suffix == ids, "Native control reading/topmost order changed")
    }

    private static func requireStable(_ host: CountingHost, _ controls: NativeControls,
                                      _ values: [[String: Any]]) throws {
        host.reorders = 0
        for _ in 0..<100 { controls.update(values) }
        try require(host.reorders == 0, "Identical native controls redundantly reordered UIKit views")
    }

    static func run() throws -> [String] {
        let host = CountingHost(frame: CGRect(x: 0, y: 0, width: 320, height: 568))
        let editor = UIView()
        editor.accessibilityLabel = "editor"
        host.addSubview(editor)
        let controls = NativeControls(host: host)
        controls.update(specs(["a", "b", "c"]))
        try requireStable(host, controls, specs(["a", "b", "c"]))
        try requireOrder(host, ["a", "b", "c"])
        try ordering(host, controls)
        try foreignViews(host, controls)
        try emptyAndDuplicateIDs(host, controls)
        try changedAppearance(host, controls)
        return ["native-control-stability"]
    }

    private static func ordering(_ host: CountingHost, _ controls: NativeControls) throws {
        controls.update(specs(["c", "a", "b"]))
        try requireOrder(host, ["c", "a", "b"])
        try requireStable(host, controls, specs(["c", "a", "b"]))
        // Overlapping real buttons: UIKit's hit-test order must follow the specs.
        guard let hit = host.hitTest(CGPoint(x: 70, y: 64), with: nil),
              let top = host.subviews.last else {
            throw Failure(description: "Overlapping native control could not be hit tested")
        }
        try require(hit === top || hit.isDescendant(of: top),
                    "Overlapping native controls changed UIKit hit-test ownership")
        controls.update(specs(["a", "c"]))
        try requireOrder(host, ["a", "c"])
        try require(controls.evidence.count == 2, "Removed native control remains mounted")
    }

    private static func foreignViews(_ host: CountingHost, _ controls: NativeControls) throws {
        let above = UIView()
        host.addSubview(above)
        controls.update(specs(["a", "c"]))
        try requireOrder(host, ["a", "c"])
        let between = UIView()
        host.insertSubview(between, at: host.subviews.count - 1)
        controls.update(specs(["a", "c"]))
        try requireOrder(host, ["a", "c"])
        try requireStable(host, controls, specs(["a", "c"]))
        try require(above.superview === host && between.superview === host,
                    "Updating native controls removed an unrelated UIKit view")
    }

    private static func emptyAndDuplicateIDs(_ host: CountingHost, _ controls: NativeControls) throws {
        controls.update([])
        try require(controls.evidence.isEmpty, "Empty specs retained native controls")
        try require(!host.subviews.contains { $0 is UIButton }, "Empty specs left a UIKit button mounted")
        try requireStable(host, controls, [])
        let duplicate = specs(["a", "b", "a", "c"])
        controls.update(duplicate)
        try requireOrder(host, ["b", "a", "c"])
        try require(controls.evidence.count == 3, "Duplicate id created duplicate UIKit buttons")
        try requireStable(host, controls, duplicate)
    }

    private static func changedAppearance(_ host: CountingHost, _ controls: NativeControls) throws {
        guard let button = host.subviews.last as? UIButton else {
            throw Failure(description: "Native control button missing")
        }
        let oldInk = button.configuration?.baseForegroundColor
        var changed = specs(["c"])
        changed[0]["rect"] = [30.0, 90.0, 140.0, 52.0]
        changed[0]["opacity"] = 0.5
        changed[0]["enabled"] = false
        changed[0]["ink"] = [1.0, 0.0, 0.0]
        controls.update(changed)
        try requireStable(host, controls, changed)
        try requireOrder(host, ["c"])
        try require(button.frame == CGRect(x: 30, y: 90, width: 140, height: 52),
                    "Changed native control geometry was ignored")
        try require(button.alpha == 0.5 && !button.isEnabled, "Changed native control state was ignored")
        try require(button.configuration?.baseForegroundColor != oldInk, "Changed native control look was ignored")
        controls.removeAll()
    }
}

#endif
