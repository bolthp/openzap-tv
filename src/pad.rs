//! Gamepad to command translation, independent of the gamepad library and
//! the UI: buttons and the left stick become the keys the app already
//! understands, plus volume and quit. Held directions and volume repeat.
//!
//! Layout (Xbox names; the Steam Deck's buttons are in the same places):
//! D-pad / left stick: arrows; A: Enter; B: Esc; X: mute; Y: programme
//! guide; LB / RB: previous / next channel; LT / RT: volume; View: channel
//! list; Menu: info; View + Menu together: quit.

/// A gamepad button, by position (south = A on Xbox pads).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    South,
    East,
    West,
    North,
    LeftBumper,
    RightBumper,
    LeftTrigger,
    RightTrigger,
    /// View / Select / Back (⧉).
    Select,
    /// Menu / Start (☰).
    Start,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
}

/// What the app should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Act as if this key was pressed (`repeat`: auto-repeat of a held one).
    Key {
        key: Key,
        repeat: bool,
    },
    VolumeUp,
    VolumeDown,
    Quit,
}

/// The keys a gamepad can stand in for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Escape,
    PageUp,
    PageDown,
    /// `M`
    Mute,
    /// `G`
    Guide,
    /// `L`
    List,
    /// `I`
    Info,
}

/// Delay before a held direction or volume button starts repeating, s.
pub const REPEAT_DELAY: f64 = 0.4;
/// Time between repeats, s.
pub const REPEAT_INTERVAL: f64 = 0.1;
/// The left stick counts as a direction beyond this deflection...
const STICK_ON: f32 = 0.6;
/// ...and lets go below this one.
const STICK_OFF: f32 = 0.35;

/// A direction or volume step that repeats while held.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Held {
    command: Command,
    next: f64,
}

/// Turns button presses, releases and stick moves into commands.
#[derive(Debug, Default)]
pub struct Pad {
    /// The repeating input from buttons, by button.
    held: Vec<(Button, Held)>,
    /// The repeating input from the left stick.
    stick: Option<Held>,
    stick_x: f32,
    stick_y: f32,
    select_down: bool,
    start_down: bool,
    /// View and Menu were pressed together: neither acts on its own.
    chord_used: bool,
}

impl Pad {
    pub fn new() -> Self {
        Self::default()
    }

    /// A button went down at time `now` (seconds).
    pub fn press(&mut self, button: Button, now: f64) -> Vec<Command> {
        let mut out = Vec::new();
        match button {
            // View and Menu act on release, so pressing both can quit.
            Button::Select | Button::Start => {
                if button == Button::Select {
                    self.select_down = true;
                } else {
                    self.start_down = true;
                }
                if self.select_down && self.start_down && !self.chord_used {
                    self.chord_used = true;
                    out.push(Command::Quit);
                }
            }
            _ => {
                if let Some(command) = command_for(button) {
                    out.push(command);
                    if repeats(command) {
                        self.held.retain(|(b, _)| *b != button);
                        self.held.push((
                            button,
                            Held {
                                command: again(command),
                                next: now + REPEAT_DELAY,
                            },
                        ));
                    }
                }
            }
        }
        out
    }

    /// A button went up.
    pub fn release(&mut self, button: Button) -> Vec<Command> {
        self.held.retain(|(b, _)| *b != button);
        let mut out = Vec::new();
        let (was_down, key) = match button {
            Button::Select => (std::mem::take(&mut self.select_down), Key::List),
            Button::Start => (std::mem::take(&mut self.start_down), Key::Info),
            _ => return out,
        };
        if was_down && !self.chord_used {
            out.push(Command::Key { key, repeat: false });
        }
        if !self.select_down && !self.start_down {
            self.chord_used = false;
        }
        out
    }

    /// The left stick moved (`x` right, `y` up, each -1..1).
    pub fn stick(&mut self, x: Option<f32>, y: Option<f32>, now: f64) -> Vec<Command> {
        if let Some(x) = x {
            self.stick_x = x;
        }
        if let Some(y) = y {
            self.stick_y = y;
        }
        let current = self.stick.map(|h| h.command);
        let wanted = stick_direction(self.stick_x, self.stick_y, current);
        if wanted == current.map(first) {
            return Vec::new();
        }
        self.stick = wanted.map(|command| Held {
            command: again(command),
            next: now + REPEAT_DELAY,
        });
        wanted.into_iter().collect()
    }

    /// Repeats of held directions and volume due at `now`.
    pub fn tick(&mut self, now: f64) -> Vec<Command> {
        let mut out = Vec::new();
        let held = self
            .held
            .iter_mut()
            .map(|(_, h)| h)
            .chain(self.stick.as_mut());
        for h in held {
            if now >= h.next {
                out.push(h.command);
                h.next = (h.next + REPEAT_INTERVAL).max(now);
            }
        }
        out
    }

    /// When [`Pad::tick`] has something to do next, if anything is held.
    pub fn next_tick(&self) -> Option<f64> {
        self.held
            .iter()
            .map(|(_, h)| h.next)
            .chain(self.stick.map(|h| h.next))
            .reduce(f64::min)
    }
}

fn command_for(button: Button) -> Option<Command> {
    let key = |key| Some(Command::Key { key, repeat: false });
    match button {
        Button::DPadUp => key(Key::Up),
        Button::DPadDown => key(Key::Down),
        Button::DPadLeft => key(Key::Left),
        Button::DPadRight => key(Key::Right),
        Button::South => key(Key::Enter),
        Button::East => key(Key::Escape),
        Button::West => key(Key::Mute),
        Button::North => key(Key::Guide),
        Button::LeftBumper => key(Key::PageUp),
        Button::RightBumper => key(Key::PageDown),
        Button::LeftTrigger => Some(Command::VolumeDown),
        Button::RightTrigger => Some(Command::VolumeUp),
        Button::Select | Button::Start => None,
    }
}

/// Directions and volume repeat while held; everything else acts once.
fn repeats(command: Command) -> bool {
    matches!(
        command,
        Command::VolumeUp
            | Command::VolumeDown
            | Command::Key {
                key: Key::Up | Key::Down | Key::Left | Key::Right,
                ..
            }
    )
}

/// The repeat of `command`.
fn again(command: Command) -> Command {
    match command {
        Command::Key { key, .. } => Command::Key { key, repeat: true },
        other => other,
    }
}

/// The first press behind a repeat.
fn first(command: Command) -> Command {
    match command {
        Command::Key { key, .. } => Command::Key { key, repeat: false },
        other => other,
    }
}

/// The arrow the stick points to: along its larger axis, once past
/// [`STICK_ON`]; a direction already held stays until below [`STICK_OFF`].
fn stick_direction(x: f32, y: f32, current: Option<Command>) -> Option<Command> {
    let key = |key| Command::Key { key, repeat: false };
    let held_on = |k| current.map(first) == Some(key(k));
    let strength = x.abs().max(y.abs());
    let pointing = if x.abs() > y.abs() {
        if x > 0.0 { Key::Right } else { Key::Left }
    } else if y > 0.0 {
        Key::Up
    } else {
        Key::Down
    };
    if strength >= STICK_ON || (held_on(pointing) && strength >= STICK_OFF) {
        Some(key(pointing))
    } else if current.is_some() && strength >= STICK_OFF {
        current.map(first)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key) -> Command {
        Command::Key { key, repeat: false }
    }

    fn rep(key: Key) -> Command {
        Command::Key { key, repeat: true }
    }

    #[test]
    fn buttons_map_to_keys_and_volume() {
        let mut pad = Pad::new();
        assert_eq!(pad.press(Button::South, 0.0), [key(Key::Enter)]);
        assert_eq!(pad.press(Button::East, 0.0), [key(Key::Escape)]);
        assert_eq!(pad.press(Button::West, 0.0), [key(Key::Mute)]);
        assert_eq!(pad.press(Button::North, 0.0), [key(Key::Guide)]);
        assert_eq!(pad.press(Button::LeftBumper, 0.0), [key(Key::PageUp)]);
        assert_eq!(pad.press(Button::RightBumper, 0.0), [key(Key::PageDown)]);
        assert_eq!(pad.press(Button::RightTrigger, 0.0), [Command::VolumeUp]);
        assert_eq!(pad.press(Button::LeftTrigger, 0.0), [Command::VolumeDown]);
    }

    #[test]
    fn held_direction_repeats_until_released() {
        let mut pad = Pad::new();
        assert_eq!(pad.press(Button::DPadDown, 1.0), [key(Key::Down)]);
        assert_eq!(pad.next_tick(), Some(1.0 + REPEAT_DELAY));
        assert!(pad.tick(1.2).is_empty());
        assert_eq!(pad.tick(1.0 + REPEAT_DELAY), [rep(Key::Down)]);
        assert!(pad.tick(1.0 + REPEAT_DELAY + 0.05).is_empty());
        assert_eq!(
            pad.tick(1.0 + REPEAT_DELAY + REPEAT_INTERVAL),
            [rep(Key::Down)]
        );
        pad.release(Button::DPadDown);
        assert_eq!(pad.next_tick(), None);
        assert!(pad.tick(5.0).is_empty());
    }

    #[test]
    fn a_late_tick_repeats_once_not_in_a_burst() {
        let mut pad = Pad::new();
        pad.press(Button::RightTrigger, 0.0);
        assert_eq!(pad.tick(3.0), [Command::VolumeUp]);
        assert_eq!(pad.next_tick(), Some(3.0));
        assert_eq!(pad.tick(3.0), [Command::VolumeUp]);
        assert!(pad.next_tick().unwrap() > 3.0);
    }

    #[test]
    fn other_buttons_do_not_repeat() {
        let mut pad = Pad::new();
        pad.press(Button::South, 0.0);
        pad.press(Button::RightBumper, 0.0);
        assert_eq!(pad.next_tick(), None);
    }

    #[test]
    fn view_and_menu_act_on_release() {
        let mut pad = Pad::new();
        assert!(pad.press(Button::Select, 0.0).is_empty());
        assert_eq!(pad.release(Button::Select), [key(Key::List)]);
        assert!(pad.press(Button::Start, 0.0).is_empty());
        assert_eq!(pad.release(Button::Start), [key(Key::Info)]);
    }

    #[test]
    fn view_and_menu_together_quit_and_nothing_else() {
        let mut pad = Pad::new();
        pad.press(Button::Start, 0.0);
        assert_eq!(pad.press(Button::Select, 0.1), [Command::Quit]);
        assert!(pad.release(Button::Start).is_empty());
        assert!(pad.release(Button::Select).is_empty());
        // Afterwards each works alone again.
        pad.press(Button::Select, 1.0);
        assert_eq!(pad.release(Button::Select), [key(Key::List)]);
    }

    #[test]
    fn a_release_without_press_does_nothing() {
        let mut pad = Pad::new();
        assert!(pad.release(Button::Start).is_empty());
        assert!(pad.release(Button::South).is_empty());
    }

    #[test]
    fn stick_acts_as_arrows_with_hysteresis() {
        let mut pad = Pad::new();
        assert!(pad.stick(Some(0.3), None, 0.0).is_empty());
        assert_eq!(pad.stick(Some(0.8), None, 0.0), [key(Key::Right)]);
        // Easing off a little keeps the direction, without a new press.
        assert!(pad.stick(Some(0.5), None, 0.1).is_empty());
        assert_eq!(pad.tick(REPEAT_DELAY), [rep(Key::Right)]);
        // Back to the middle lets go.
        assert!(pad.stick(Some(0.1), None, 0.5).is_empty());
        assert_eq!(pad.next_tick(), None);
        // Up and down: y is up.
        assert_eq!(pad.stick(Some(0.0), Some(0.9), 1.0), [key(Key::Up)]);
        assert_eq!(pad.stick(None, Some(-0.9), 1.1), [key(Key::Down)]);
    }

    #[test]
    fn stick_turning_to_another_axis_changes_direction() {
        let mut pad = Pad::new();
        pad.stick(Some(0.9), Some(0.0), 0.0);
        assert_eq!(pad.stick(Some(0.1), Some(-0.9), 0.1), [key(Key::Down)]);
    }
}
