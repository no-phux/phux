import SwiftUI

/// phux's fox, looking: wide open in the needs-you colour while an agent
/// waits, narrowed while agents work, shut when nobody has looked lately.
struct FoxEyes: View {
    enum Mood { case alert, busy, asleep }
    let mood: Mood

    init(_ mood: Mood) { self.mood = mood }
    init(_ presence: AgentPresence, stale: Bool = false) {
        mood = stale || presence.quiet ? .asleep : presence.waiting > 0 ? .alert : .busy
    }

    var body: some View {
        HStack(spacing: 5) {
            eye
            eye.scaleEffect(x: -1)
        }
        .aspectRatio(2.6, contentMode: .fit)
        .accessibilityHidden(true)
    }

    private var eye: some View {
        Eye(open: open)
            .fill(ink, style: FillStyle(eoFill: true))
            .overlay { if mood == .asleep { Eye(open: 0).stroke(ink, style: StrokeStyle(lineWidth: 2, lineCap: .round)) } }
            .widgetAccentable()
    }

    private var open: CGFloat {
        switch mood {
        case .alert: 1
        case .busy: 0.45
        case .asleep: 0
        }
    }

    private var ink: Color {
        switch mood {
        case .alert: Color("StatusNeedsYou")
        case .busy: Color("StatusWorking")
        case .asleep: .secondary
        }
    }
}

/// The logo's left eye: an almond that slants down to the nose, with a
/// fox's upright slit while it is open enough to show one.
private struct Eye: Shape {
    var open: CGFloat
    var animatableData: CGFloat {
        get { open }
        set { open = newValue }
    }

    func path(in rect: CGRect) -> Path {
        func at(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
            CGPoint(x: rect.minX + x * rect.width, y: rect.minY + y * rect.height)
        }
        let outer = at(0, 0.22), inner = at(1, 0.8)
        var path = Path()
        path.move(to: outer)
        if open == 0 {
            path.addQuadCurve(to: inner, control: at(0.42, 0.95))
            return path
        }
        path.addCurve(to: inner, control1: at(0.45, 0.5 - 0.56 * open), control2: at(0.9, 0.62 - 0.5 * open))
        path.addCurve(to: outer, control1: at(0.72, 0.8 + 0.3 * open), control2: at(0.2, 0.6 + 0.5 * open))
        path.closeSubpath()
        if open > 0.7 {
            let slit = CGSize(width: rect.width * 0.14, height: rect.height * 0.46)
            let centre = at(0.55, 0.5)
            path.addEllipse(in: CGRect(x: centre.x - slit.width / 2, y: centre.y - slit.height / 2,
                                       width: slit.width, height: slit.height))
        }
        return path
    }
}
