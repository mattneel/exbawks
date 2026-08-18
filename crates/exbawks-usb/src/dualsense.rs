//! Turning a Sony controller's report into the console's own.
//!
//! ADR 0019 has the host device supply button and axis values and nothing
//! more: a DualSense does not present the descriptors, endpoints, or report
//! the title's driver is written against, so what it sends is translated
//! here rather than forwarded.
//!
//! The translation is pure and lives beside the guest-side model so it can
//! be tested without a controller attached, which is how the mapping below
//! is checked.

use crate::gamepad::{GamepadState, analogue, button};

/// The report identifier a wired controller sends its state under.
const WIRED_REPORT: u8 = 0x01;

/// Where each field sits in that report.
mod field {
    pub const LEFT_X: usize = 1;
    pub const LEFT_Y: usize = 2;
    pub const RIGHT_X: usize = 3;
    pub const RIGHT_Y: usize = 4;
    pub const LEFT_TRIGGER: usize = 5;
    pub const RIGHT_TRIGGER: usize = 6;
    /// The direction pad and the four face buttons.
    pub const FACE: usize = 8;
    /// The shoulders, the sticks' clicks, and the two menu buttons.
    pub const SHOULDER: usize = 9;
}

/// The face-button bits, above the direction pad's four.
const SQUARE: u8 = 1 << 4;
const CROSS: u8 = 1 << 5;
const CIRCLE: u8 = 1 << 6;
const TRIANGLE: u8 = 1 << 7;

/// The shoulder byte's bits.
const LEFT_SHOULDER: u8 = 1 << 0;
const RIGHT_SHOULDER: u8 = 1 << 1;
const CREATE: u8 = 1 << 4;
const OPTIONS: u8 = 1 << 5;
const LEFT_STICK: u8 = 1 << 6;
const RIGHT_STICK: u8 = 1 << 7;

/// The direction pad's eight positions, and the ninth meaning centred.
/// Anything at or above it is read as no direction at all.
#[cfg_attr(not(test), expect(dead_code, reason = "documents the encoding the match below reads"))]
const DPAD_CENTRED: u8 = 8;

/// Widens an unsigned stick axis to the signed range the console uses.
///
/// A resting stick reads at the middle of its range, which has to land on
/// zero rather than near it, or a title reads a permanent slight lean.
fn axis(value: u8) -> i16 {
    let centred = i32::from(value) - 128;
    // Scale so a full deflection reaches the end of the signed range.
    let scaled = centred * 258;
    scaled.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// The direction pad's bits for one of its nine positions.
fn direction(position: u8) -> u16 {
    match position {
        0 => button::UP,
        1 => button::UP | button::RIGHT,
        2 => button::RIGHT,
        3 => button::DOWN | button::RIGHT,
        4 => button::DOWN,
        5 => button::DOWN | button::LEFT,
        6 => button::LEFT,
        7 => button::UP | button::LEFT,
        _ => 0,
    }
}

/// Translates one controller report, or reports `None` for one this does
/// not recognise — a report identifier it does not know, or one too short
/// to hold the fields it needs.
#[must_use]
pub fn translate(report: &[u8]) -> Option<GamepadState> {
    if report.first().copied()? != WIRED_REPORT || report.len() <= field::SHOULDER {
        return None;
    }
    let face = report[field::FACE];
    let shoulder = report[field::SHOULDER];

    let mut buttons = direction(face & 0xF);
    for (bit, mapped) in [
        (OPTIONS, button::START),
        (CREATE, button::BACK),
        (LEFT_STICK, button::LEFT_STICK),
        (RIGHT_STICK, button::RIGHT_STICK),
    ] {
        if shoulder & bit != 0 {
            buttons |= mapped;
        }
    }

    // The face buttons are pressure-sensitive on the console's own pad, so
    // a press is reported as full pressure rather than as a bit.
    let mut pressures = [0_u8; 8];
    for (bit, index) in [
        (CROSS, analogue::A),
        (CIRCLE, analogue::B),
        (SQUARE, analogue::X),
        (TRIANGLE, analogue::Y),
    ] {
        if face & bit != 0 {
            pressures[index] = u8::MAX;
        }
    }
    // The shoulders take the console's two spare buttons, which is where a
    // title of this era expects its remaining pair.
    if shoulder & LEFT_SHOULDER != 0 {
        pressures[analogue::WHITE] = u8::MAX;
    }
    if shoulder & RIGHT_SHOULDER != 0 {
        pressures[analogue::BLACK] = u8::MAX;
    }
    pressures[analogue::LEFT_TRIGGER] = report[field::LEFT_TRIGGER];
    pressures[analogue::RIGHT_TRIGGER] = report[field::RIGHT_TRIGGER];

    Some(GamepadState {
        buttons,
        analogue: pressures,
        // The vertical axes are inverted: a controller reports downward as
        // increasing, and the console reports upward as increasing.
        left_stick: (axis(report[field::LEFT_X]), axis(report[field::LEFT_Y]).saturating_neg()),
        right_stick: (axis(report[field::RIGHT_X]), axis(report[field::RIGHT_Y]).saturating_neg()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A report with everything at rest.
    fn resting() -> Vec<u8> {
        let mut report = vec![0_u8; 64];
        report[0] = WIRED_REPORT;
        for index in [field::LEFT_X, field::LEFT_Y, field::RIGHT_X, field::RIGHT_Y] {
            report[index] = 128;
        }
        report[field::FACE] = DPAD_CENTRED;
        report
    }

    #[test]
    fn a_resting_controller_reports_nothing_pressed() {
        let state = translate(&resting()).expect("a wired report translates");
        assert_eq!(state.buttons, 0);
        assert_eq!(state.analogue, [0; 8]);
        // A centred stick has to land exactly on zero: a title reads any
        // residue as a permanent lean.
        assert_eq!(state.left_stick, (0, 0));
        assert_eq!(state.right_stick, (0, 0));
    }

    #[test]
    fn the_menu_buttons_map_to_start_and_back() {
        let mut report = resting();
        report[field::SHOULDER] = OPTIONS;
        let state = translate(&report).expect("translates");
        assert_eq!(
            state.buttons,
            button::START,
            "options is start, which is what a title asks for"
        );

        report[field::SHOULDER] = CREATE;
        assert_eq!(translate(&report).expect("translates").buttons, button::BACK);
    }

    #[test]
    fn the_face_buttons_report_pressure() {
        let mut report = resting();
        report[field::FACE] = DPAD_CENTRED | CROSS;
        let state = translate(&report).expect("translates");
        assert_eq!(state.analogue[analogue::A], u8::MAX, "cross is the accept button");
        assert_eq!(state.analogue[analogue::B], 0);

        report[field::FACE] = DPAD_CENTRED | CIRCLE | TRIANGLE;
        let state = translate(&report).expect("translates");
        assert_eq!(state.analogue[analogue::B], u8::MAX);
        assert_eq!(state.analogue[analogue::Y], u8::MAX);
        assert_eq!(state.analogue[analogue::A], 0);
    }

    #[test]
    fn the_direction_pad_reads_as_its_eight_positions() {
        let mut report = resting();
        for (position, expected) in [
            (0, button::UP),
            (2, button::RIGHT),
            (4, button::DOWN),
            (6, button::LEFT),
            (1, button::UP | button::RIGHT),
            (5, button::DOWN | button::LEFT),
        ] {
            report[field::FACE] = position;
            assert_eq!(translate(&report).expect("translates").buttons, expected, "at {position}");
        }
        report[field::FACE] = DPAD_CENTRED;
        assert_eq!(translate(&report).expect("translates").buttons, 0, "centred is nothing");
    }

    #[test]
    fn the_triggers_pass_their_pressure_through() {
        let mut report = resting();
        report[field::LEFT_TRIGGER] = 200;
        report[field::RIGHT_TRIGGER] = 30;
        let state = translate(&report).expect("translates");
        assert_eq!(state.analogue[analogue::LEFT_TRIGGER], 200);
        assert_eq!(state.analogue[analogue::RIGHT_TRIGGER], 30);
    }

    #[test]
    fn the_sticks_reach_both_ends_and_point_the_right_way() {
        let mut report = resting();
        report[field::LEFT_X] = 255;
        report[field::LEFT_Y] = 0;
        let state = translate(&report).expect("translates");
        assert!(state.left_stick.0 > 30_000, "right is positive: {}", state.left_stick.0);
        assert!(state.left_stick.1 > 30_000, "and up is positive too: {}", state.left_stick.1);

        report[field::LEFT_X] = 0;
        report[field::LEFT_Y] = 255;
        let state = translate(&report).expect("translates");
        assert!(state.left_stick.0 < -30_000, "left is negative");
        assert!(state.left_stick.1 < -30_000, "and down is negative");
    }

    #[test]
    fn a_report_this_does_not_know_is_refused() {
        // A controller sends other reports — its motion sensors, its
        // audio — and translating one of those would invent input.
        let mut report = resting();
        report[0] = 0x31;
        assert!(translate(&report).is_none(), "an unknown identifier is refused");
        assert!(translate(&[]).is_none(), "and so is nothing at all");
        assert!(translate(&[WIRED_REPORT, 0, 0]).is_none(), "and a truncated report");
    }
}
