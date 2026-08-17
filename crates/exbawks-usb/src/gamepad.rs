//! The synthetic Xbox gamepad the guest enumerates.
//!
//! ADR 0019 describes it as a device of its own rather than a pass-through
//! of whatever the host has plugged in: a title's driver is written against
//! the Xbox controller's descriptors and report layout, so the host device
//! is a source of button and axis values and nothing more.

/// The buttons a report carries in its digital field.
///
/// The Xbox controller splits its buttons: the directions and the four
/// small buttons are one bit each, and the four face buttons and two
/// triggers are a byte of pressure each.
pub mod button {
    /// Directional pad, one bit each.
    pub const UP: u16 = 1 << 0;
    pub const DOWN: u16 = 1 << 1;
    pub const LEFT: u16 = 1 << 2;
    pub const RIGHT: u16 = 1 << 3;
    /// Start and Back.
    pub const START: u16 = 1 << 4;
    pub const BACK: u16 = 1 << 5;
    /// The two stick clicks.
    pub const LEFT_STICK: u16 = 1 << 6;
    pub const RIGHT_STICK: u16 = 1 << 7;
}

/// The analogue buttons, in the order the report stores them.
pub mod analogue {
    /// Index of each pressure byte within the report's analogue field.
    pub const A: usize = 0;
    pub const B: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    pub const BLACK: usize = 4;
    pub const WHITE: usize = 5;
    pub const LEFT_TRIGGER: usize = 6;
    pub const RIGHT_TRIGGER: usize = 7;
}

/// One sample of a controller's state, in the Xbox's own units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GamepadState {
    /// The digital buttons, as a mask of the `button` constants.
    pub buttons: u16,
    /// The eight analogue buttons' pressures, each `0..=255`.
    pub analogue: [u8; 8],
    /// The left stick, each axis `-32768..=32767`.
    pub left_stick: (i16, i16),
    /// The right stick.
    pub right_stick: (i16, i16),
}

/// The bytes one gamepad report occupies.
pub const REPORT_BYTES: usize = 20;

impl GamepadState {
    /// Encodes this state as the twenty-byte report the guest reads.
    ///
    /// The layout is the controller's own: a type byte, the report's
    /// length, the digital buttons, a pad byte, the eight pressures, and
    /// the two sticks as signed little-endian pairs.
    #[must_use]
    pub fn report(&self) -> [u8; REPORT_BYTES] {
        let mut bytes = [0_u8; REPORT_BYTES];
        bytes[0] = 0x00;
        bytes[1] = REPORT_BYTES as u8;
        bytes[2] = (self.buttons & 0xFF) as u8;
        bytes[3] = 0x00;
        bytes[4..12].copy_from_slice(&self.analogue);
        bytes[12..14].copy_from_slice(&self.left_stick.0.to_le_bytes());
        bytes[14..16].copy_from_slice(&self.left_stick.1.to_le_bytes());
        bytes[16..18].copy_from_slice(&self.right_stick.0.to_le_bytes());
        bytes[18..20].copy_from_slice(&self.right_stick.1.to_le_bytes());
        bytes
    }
}

/// Where a run's controller state comes from.
///
/// The default is nothing at all, which keeps a run a pure function of the
/// image — the property every recorded frame digest depends on.
pub trait InputSource: Send + Sync {
    /// The controller's state for the frame `frame`, or `None` when no
    /// controller is attached.
    fn sample(&self, frame: u64) -> Option<GamepadState>;

    /// Whether this source makes a run reproducible. A golden may only be
    /// recorded from a run whose sources all say yes.
    fn deterministic(&self) -> bool;
}

/// An input source with no controller attached.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoInput;

impl InputSource for NoInput {
    fn sample(&self, _frame: u64) -> Option<GamepadState> {
        None
    }

    fn deterministic(&self) -> bool {
        true
    }
}

/// A recorded sequence of states, advanced by frame number.
///
/// This is what a test involving input uses: the same run produces the same
/// buttons at the same frames, every time.
#[derive(Debug, Default, Clone)]
pub struct ScriptedInput {
    /// Each entry is the first frame a state applies from, and that state.
    /// The entries are held in ascending order of frame.
    steps: Vec<(u64, GamepadState)>,
}

impl ScriptedInput {
    /// Builds a script from `(frame, state)` pairs in any order.
    #[must_use]
    pub fn new(mut steps: Vec<(u64, GamepadState)>) -> Self {
        steps.sort_by_key(|(frame, _)| *frame);
        Self { steps }
    }

    /// A script that holds one button from a given frame onwards.
    #[must_use]
    pub fn press(frame: u64, buttons: u16) -> Self {
        Self::new(vec![
            (0, GamepadState::default()),
            (frame, GamepadState { buttons, ..GamepadState::default() }),
        ])
    }
}

impl InputSource for ScriptedInput {
    fn sample(&self, frame: u64) -> Option<GamepadState> {
        if self.steps.is_empty() {
            return None;
        }
        // The last entry whose frame has been reached.
        let index = self.steps.partition_point(|(at, _)| *at <= frame);
        Some(if index == 0 { self.steps[0].1 } else { self.steps[index - 1].1 })
    }

    fn deterministic(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_carries_its_buttons_and_sticks() {
        let state = GamepadState {
            buttons: button::START | button::UP,
            analogue: [0, 0, 0, 0, 0, 0, 200, 0],
            left_stick: (-16384, 16384),
            right_stick: (0, -1),
        };
        let report = state.report();

        assert_eq!(report[1], REPORT_BYTES as u8, "the report states its own length");
        assert_eq!(report[2], (button::START | button::UP) as u8);
        assert_eq!(report[4 + analogue::LEFT_TRIGGER], 200, "the trigger's pressure");
        assert_eq!(i16::from_le_bytes([report[12], report[13]]), -16384);
        assert_eq!(i16::from_le_bytes([report[14], report[15]]), 16384);
        assert_eq!(i16::from_le_bytes([report[18], report[19]]), -1);
    }

    #[test]
    fn no_input_reports_an_empty_port() {
        assert_eq!(NoInput.sample(0), None);
        assert!(NoInput.deterministic(), "an empty port keeps a run reproducible");
    }

    #[test]
    fn a_script_holds_each_state_until_the_next() {
        let script = ScriptedInput::press(100, button::START);

        assert_eq!(script.sample(0).expect("attached").buttons, 0);
        assert_eq!(script.sample(99).expect("attached").buttons, 0);
        assert_eq!(
            script.sample(100).expect("attached").buttons,
            button::START,
            "the press lands on its own frame"
        );
        assert_eq!(
            script.sample(10_000).expect("attached").buttons,
            button::START,
            "and holds after it"
        );
        assert!(script.deterministic(), "a script is reproducible");
    }

    #[test]
    fn a_script_given_out_of_order_still_reads_in_order() {
        let held = GamepadState { buttons: button::LEFT_STICK, ..GamepadState::default() };
        let script = ScriptedInput::new(vec![
            (50, held),
            (10, GamepadState { buttons: button::BACK, ..GamepadState::default() }),
        ]);
        assert_eq!(script.sample(10).expect("attached").buttons, button::BACK);
        assert_eq!(script.sample(50).expect("attached").buttons, held.buttons);
    }
}
