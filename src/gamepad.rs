//! Reads gamepads with `gilrs` on a thread of its own and hands the app
//! the commands of [`openzap::pad`]. The thread waits for gamepad events,
//! so the UI is woken up (repainted) only when something happened.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use gilrs::{Axis, EventType, Gilrs};
use openzap::pad::{self, Command, Pad};

/// What the gamepad thread reports.
#[derive(Debug)]
pub enum Report {
    Command(Command),
    /// A line for the debug log.
    Note(String),
}

/// The receiving end; the thread ends when this is dropped.
pub struct Gamepads {
    rx: Receiver<Report>,
}

impl Gamepads {
    /// Starts watching gamepads; `ctx` is repainted on every report.
    pub fn start(ctx: &egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("gamepad".into())
            .spawn(move || run(&tx, &ctx));
        let gamepads = Gamepads { rx };
        if let Err(e) = spawned {
            // The receiver reports nothing then; the app works without.
            eprintln!("gamepad thread: {e}");
        }
        gamepads
    }

    /// Reports that arrived since the last call.
    pub fn take(&self) -> Vec<Report> {
        self.rx.try_iter().collect()
    }
}

fn run(tx: &Sender<Report>, ctx: &egui::Context) {
    let send = |report: Report| {
        let ok = tx.send(report).is_ok();
        ctx.request_repaint();
        ok
    };
    let mut gilrs = match Gilrs::new() {
        Ok(g) => g,
        Err(e) => {
            send(Report::Note(format!("gamepad: unavailable: {e}")));
            return;
        }
    };
    if gilrs.gamepads().next().is_none() {
        // Pads connected later are reported then.
        if !send(Report::Note("gamepad: none connected yet".into())) {
            return;
        }
    }
    for (_, gamepad) in gilrs.gamepads() {
        let line = format!(
            "gamepad: {} (mapping: {:?})",
            gamepad.name(),
            gamepad.mapping_source()
        );
        if !send(Report::Note(line)) {
            return;
        }
    }
    let start = Instant::now();
    let mut pad = Pad::new();
    loop {
        let now = start.elapsed().as_secs_f64();
        // Wake up for the next repeat, or wait for an event.
        let timeout = pad
            .next_tick()
            .map(|at| Duration::from_secs_f64((at - now).max(0.0)));
        let event = gilrs.next_event_blocking(timeout);
        let now = start.elapsed().as_secs_f64();
        let mut commands = pad.tick(now);
        if let Some(event) = event {
            match event.event {
                EventType::ButtonPressed(b, _) => {
                    commands.extend(button(b).map_or_else(Vec::new, |b| pad.press(b, now)));
                }
                EventType::ButtonReleased(b, _) => {
                    commands.extend(button(b).map_or_else(Vec::new, |b| pad.release(b)));
                }
                EventType::AxisChanged(Axis::LeftStickX, v, _) => {
                    commands.extend(pad.stick(Some(v), None, now));
                }
                EventType::AxisChanged(Axis::LeftStickY, v, _) => {
                    commands.extend(pad.stick(None, Some(v), now));
                }
                EventType::Connected => {
                    let g = gilrs.gamepad(event.id);
                    let line = format!(
                        "gamepad: connected {} (mapping: {:?})",
                        g.name(),
                        g.mapping_source()
                    );
                    if !send(Report::Note(line)) {
                        return;
                    }
                }
                EventType::Disconnected => {
                    // Nothing stays held on a pad that is gone.
                    pad = Pad::new();
                    if !send(Report::Note("gamepad: disconnected".into())) {
                        return;
                    }
                }
                _ => {}
            }
        }
        for command in commands {
            if !send(Report::Command(command)) {
                return; // the app is gone
            }
        }
    }
}

/// gilrs' button names in [`pad::Button`]s (gilrs calls the bumpers
/// "triggers" and the triggers "triggers 2"); the Guide/Steam button and
/// stick clicks are not used.
fn button(b: gilrs::Button) -> Option<pad::Button> {
    use gilrs::Button as G;
    use pad::Button as P;
    Some(match b {
        G::South => P::South,
        G::East => P::East,
        G::West => P::West,
        G::North => P::North,
        G::LeftTrigger => P::LeftBumper,
        G::RightTrigger => P::RightBumper,
        G::LeftTrigger2 => P::LeftTrigger,
        G::RightTrigger2 => P::RightTrigger,
        G::Select => P::Select,
        G::Start => P::Start,
        G::DPadUp => P::DPadUp,
        G::DPadDown => P::DPadDown,
        G::DPadLeft => P::DPadLeft,
        G::DPadRight => P::DPadRight,
        _ => return None,
    })
}

/// The egui key a gamepad key stands for.
pub fn egui_key(key: pad::Key) -> egui::Key {
    use pad::Key as K;
    match key {
        K::Up => egui::Key::ArrowUp,
        K::Down => egui::Key::ArrowDown,
        K::Left => egui::Key::ArrowLeft,
        K::Right => egui::Key::ArrowRight,
        K::Enter => egui::Key::Enter,
        K::Escape => egui::Key::Escape,
        K::PageUp => egui::Key::PageUp,
        K::PageDown => egui::Key::PageDown,
        K::Mute => egui::Key::M,
        K::Guide => egui::Key::G,
        K::List => egui::Key::L,
        K::Info => egui::Key::I,
    }
}

/// A press and release of `key`, as if typed on a keyboard.
pub fn key_events(key: egui::Key, repeat: bool) -> [egui::Event; 2] {
    let event = |pressed| egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: repeat && pressed,
        modifiers: egui::Modifiers::NONE,
    };
    [event(true), event(false)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bumpers_and_triggers_are_not_mixed_up() {
        assert_eq!(
            button(gilrs::Button::LeftTrigger),
            Some(pad::Button::LeftBumper)
        );
        assert_eq!(
            button(gilrs::Button::RightTrigger2),
            Some(pad::Button::RightTrigger)
        );
        assert_eq!(button(gilrs::Button::Mode), None);
    }

    #[test]
    fn a_key_is_pressed_and_released() {
        let [down, up] = key_events(egui::Key::ArrowDown, true);
        assert!(matches!(
            down,
            egui::Event::Key {
                key: egui::Key::ArrowDown,
                pressed: true,
                repeat: true,
                ..
            }
        ));
        assert!(matches!(
            up,
            egui::Event::Key {
                pressed: false,
                repeat: false,
                ..
            }
        ));
        assert_eq!(egui_key(pad::Key::Guide), egui::Key::G);
    }

    #[test]
    fn injected_keys_count_as_pressed() {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: key_events(egui::Key::PageDown, false).to_vec(),
            ..Default::default()
        };
        let mut pressed = false;
        ctx.run_ui(input, |ui| {
            pressed = ui.input(|i| i.key_pressed(egui::Key::PageDown));
        })
        .textures_delta
        .clear();
        assert!(pressed);
    }
}
