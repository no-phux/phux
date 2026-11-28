//! The sheets that rest on the bottom edge go the way the others do.
use super::*;

/// The sheets that rest on the bottom edge (Machines, What's new, rename)
/// lower on a swipe down from their header as their close does, never
/// from their field, and the opening press lands no input in the
/// terminal.
#[test]
fn bottom_sheets_lower_on_a_downward_swipe_as_their_close_does() {
    use crate::desk::phone::dismiss::Bottom;
    let (client, mut rt) = fixture();
    detailed_inventory(&mut rt);
    rt.set_system_reduced_motion(true);
    type Open = fn(&mut Desk);
    let opens: [(Bottom, Open); 3] = [
        (Bottom::News, Desk::toggle_news),
        (Bottom::Machines, |d| {
            // Never spawn an installed CLI in a unit test.
            d.machines.phux = None;
            d.toggle_machines();
        }),
        (Bottom::Rename, |d| {
            let key = phux_protocol::ids::SessionKey::new([1; 16]).unwrap();
            d.rename_session(key, "phux");
        }),
    ];
    for (at, (sheet, open)) in opens.into_iter().enumerate() {
        let at = at as f64 * 4.0;
        rt.update(|d, tx| {
            open(d);
            tx.instant();
        });
        settle(&mut rt);
        assert_eq!(rt.state.swipe_away(), Some(Away::Bottom(sheet)));
        // The grabber says it pulls down; its header is the handle.
        assert!(rt.find("phone-sheet-grabber").is_some());
        let panel = crate::ui::element(&rt, sheet.key()).rect;
        let header = [panel[0] + panel[2] / 3.0, panel[1] + 24.0];
        if sheet == Bottom::Rename {
            // Its field's drags edit: no swipe starts there.
            let field = centre(&rt, "rename-field");
            drag(&mut rt, field, [0.0, 200.0], at + 1.0);
            assert_eq!(rt.state.swipe_away(), Some(Away::Bottom(sheet)));
        }
        // Short of its point, 30% of its own height, it stays; past it, it
        // goes.
        drag(&mut rt, header, [0.0, panel[3] * 0.2], at + 2.0);
        assert_eq!(rt.state.swipe_away(), Some(Away::Bottom(sheet)));
        drag(&mut rt, header, [0.0, panel[3] * 0.4], at + 3.0);
        assert_eq!(rt.state.swipe_away(), None, "{sheet:?} lowered");
        assert_eq!(rt.state.modal(), None, "{sheet:?} closed");
    }
    no_terminal_input(&client);
}
