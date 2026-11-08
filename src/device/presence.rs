//! Is an Elektron machine plugged in? A background thread looks at the MIDI
//! ports every [`SCAN_EVERY`], independent of the device worker (which may be
//! busy with a long transfer), so the UI can dim device features the moment
//! the cable comes out and light them again when it goes back in.
//!
//! This only looks at port names; it never talks to the device.

use std::sync::Mutex;
use std::time::Duration;

const SCAN_EVERY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Presence {
    /// No scan has finished yet. Treated as present, so nothing flickers dim
    /// at startup.
    #[default]
    Unknown,
    /// No Elektron-looking MIDI port.
    Absent,
    /// Exactly one, by port name.
    One(String),
    /// More than one: we can't yet tell which one the user means.
    Many(Vec<String>),
}

impl Presence {
    /// Device features should look usable.
    pub fn available(&self) -> bool {
        !matches!(self, Presence::Absent)
    }

    /// Hover text for anything drawn dimmed or in error because of this.
    pub fn problem(&self) -> Option<String> {
        match self {
            Presence::Absent => Some(
                "No Elektron device found on USB MIDI. Plug it in (USB config: Overbridge or USB MIDI); \
                 this is checked every 2 seconds."
                    .into(),
            ),
            Presence::Many(names) => Some(format!(
                "More than one Elektron device is connected ({}). Only one at a time is supported: unplug the others.",
                names.join(", ")
            )),
            _ => None,
        }
    }
}

static STATE: Mutex<Presence> = Mutex::new(Presence::Unknown);

/// The latest scan.
pub fn presence() -> Presence {
    STATE.lock().map(|s| s.clone()).unwrap_or_default()
}

/// Start the scan loop. Repaints the UI whenever the answer changes.
pub fn start(ctx: egui::Context) {
    let _ = std::thread::Builder::new()
        .name("device presence".into())
        .spawn(move || {
            loop {
                let now = scan();
                let changed = match STATE.lock() {
                    Ok(mut s) if *s != now => {
                        crate::log::line(format!("device: presence {now:?}"));
                        *s = now;
                        true
                    }
                    _ => false,
                };
                if changed {
                    ctx.request_repaint();
                }
                std::thread::sleep(SCAN_EVERY);
            }
        });
}

fn scan() -> Presence {
    // A fresh client each time: a long-lived one may not see ports come and
    // go without a run loop.
    let Ok(input) = midir::MidiInput::new("Sample Manager presence") else {
        return Presence::Unknown;
    };
    let mut names: Vec<String> = input
        .ports()
        .iter()
        .filter_map(|p| input.port_name(p).ok())
        .filter(|n| super::conn::elektron_port(n))
        .collect();
    names.sort();
    names.dedup();
    match names.len() {
        0 => Presence::Absent,
        1 => Presence::One(names.remove(0)),
        _ => Presence::Many(names),
    }
}
