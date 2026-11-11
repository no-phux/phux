use super::*;
use crate::attach::render::ReplicaWalk;

const PANE: fn() -> ResourceId = || ResourceId::local(1);

/// A pane showing `count` incompressible `side`x`side` RGBA images, painted.
fn pane_with_images(count: u32, side: u32) -> HashMap<ResourceId, PaneSlot> {
    let mut slot = PaneSlot::new_with_size(10, 5).expect("slot");
    let mut seed = 0x2545_f491_u32;
    for id in 1..=count {
        let pixels: Vec<u8> = (0..side * side * 4)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed.to_be_bytes()[0]
            })
            .collect();
        let mut command =
            format!("\x1b[1;1H\x1b_Ga=T,f=32,s={side},v={side},i={id},c=1,r=1,C=1,q=2;");
        command.push_str(&base64(&pixels));
        command.push_str("\x1b\\");
        slot.terminal.vt_write(command.as_bytes());
    }
    let mut panes = HashMap::from([(PANE(), slot)]);
    let _ = paint(&mut panes);
    panes
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(TABLE[(n >> (18 - 6 * i)) as usize & 0x3f]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn paint(panes: &mut HashMap<ResourceId, PaneSlot>) -> Vec<u8> {
    let slot = panes.get_mut(&PANE()).expect("slot");
    let mut out = Vec::new();
    slot.renderer
        .render(ReplicaWalk::for_test(&slot.terminal), &mut out)
        .expect("render");
    out
}

fn transmits(bytes: &[u8]) -> usize {
    bytes.windows(6).filter(|w| *w == b"\x1b_Ga=t").count()
}

#[test]
fn the_timer_re_sends_an_image_a_heal_interval_after_its_send() {
    let before = Instant::now();
    let mut panes = pane_with_images(1, 1);
    let mut heal = KittyHeal::default();
    let due = heal.deadline(&panes).expect("an image is shown");
    assert!(due >= before + HEAL_INTERVAL);
    assert!(due <= Instant::now() + HEAL_INTERVAL);

    assert!(!heal.heal_due(&mut panes, Instant::now()), "not due yet");
    assert!(paint(&mut panes).is_empty());
    let later = due + Duration::from_millis(1);
    assert!(heal.heal_due(&mut panes, later));
    assert_eq!(transmits(&paint(&mut panes)), 1);
    assert!(heal.deadline(&panes).is_some_and(|next| next > due));
    assert!(paint(&mut panes).is_empty());
}

#[test]
fn an_unfocused_terminal_waits_for_focus_in() {
    let mut panes = pane_with_images(1, 1);
    let mut heal = KittyHeal::default();
    let later = Instant::now() + HEAL_INTERVAL + Duration::from_secs(1);
    heal.note_focus(false);
    assert!(heal.deadline(&panes).is_none());
    assert!(!heal.heal_due(&mut panes, later));
    heal.note_focus(true);
    assert!(heal.heal_on_focus(&mut panes, later));
    assert_eq!(transmits(&paint(&mut panes)), 1);
}

#[test]
fn focus_in_skips_images_sent_moments_ago() {
    let mut panes = pane_with_images(1, 1);
    let mut heal = KittyHeal::default();
    heal.note_focus(true);
    assert!(!heal.heal_on_focus(&mut panes, Instant::now()));
    assert!(paint(&mut panes).is_empty());
    let away = Instant::now() + FOCUS_MIN_AGE + Duration::from_millis(1);
    assert!(heal.heal_on_focus(&mut panes, away));
    assert_eq!(transmits(&paint(&mut panes)), 1);
}

/// Forty 64x64 images that do not compress (about 22 KB each, 870 KB in
/// all) heal a budget at a time, oldest first, a spacing apart.
#[test]
fn a_heal_re_sends_at_most_its_budget_and_owes_the_rest() {
    const COUNT: u32 = 40;
    let mut panes = pane_with_images(COUNT, 64);
    // Every image was sent before this; the ones re-sent below come after.
    let cutoff = Instant::now();
    let mut heal = KittyHeal::default();
    let mut now = cutoff;
    let (mut sent, mut heals) = (0_u32, 0);
    while heal.heal(&mut panes, now, Some(cutoff)) {
        let bytes = paint(&mut panes);
        let images = transmits(&bytes);
        assert!(images >= 1);
        assert!(
            bytes.len() <= HEAL_BUDGET + 32 * 1024,
            "{} bytes",
            bytes.len()
        );
        sent += u32::try_from(images).unwrap();
        heals += 1;
        assert!(!heal.heal(&mut panes, now, Some(cutoff)), "spaced");
        if heal.owed.is_some() {
            assert_eq!(heal.deadline(&panes), Some(now + HEAL_SPACING));
        }
        now += HEAL_SPACING;
    }
    assert_eq!(sent, COUNT, "every image healed once");
    assert!(heals >= 4, "{heals} heals");
    assert!(heal.owed.is_none());
}
