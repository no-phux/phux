import Foundation

/// A failed drawable is still an owed frame. Back off, but never abandon it
/// after a fixed number of attempts or spin the display link at 120 Hz.
struct FrameCadence {
    private var failures = 0
    mutating func reset() { failures = 0 }

    func acceptsWake(timerScheduled: Bool) -> Bool {
        failures == 0 || !timerScheduled
    }

    mutating func nextWake(drew: Bool, after: Double?) -> Double? {
        var deadline = after.flatMap { $0.isFinite ? max(0.001, $0) : nil }
        if drew { reset(); return deadline }
        let delays = [0.05, 0.1, 0.2, 0.25]
        let retry = delays[failures]
        failures = min(failures + 1, delays.count - 1)
        deadline = min(deadline ?? retry, retry)
        return deadline
    }
}
