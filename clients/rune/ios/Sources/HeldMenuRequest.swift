/// One presentation, captured by its own action and disappearance closures.
/// UIKit does not guarantee their order: disappearance alone cannot revoke a
/// selection. Explicit Rust/source invalidation does, before dismissing UIKit.
@MainActor
final class HeldMenuRequest {
    let id: UInt64
    private(set) var selected = false
    private(set) var visualEnded = false
    private(set) var invalidated = false

    init(_ id: UInt64) { self.id = id }

    func select() -> Bool {
        guard !invalidated, !selected else { return false }
        selected = true
        return true
    }

    func disappear() -> Bool {
        guard !invalidated, !visualEnded else { return false }
        visualEnded = true
        return !selected
    }

    func invalidate() { invalidated = true }
}
