// Be able to use cool `|` syntax in matches.
#![feature(or_patterns)]

extern crate midir;
extern crate tokio;

use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::cmp::{Eq, PartialEq, min};
use std::hash::{Hash, Hasher};
use tokio::stream::{StreamExt, StreamMap};
use tokio::sync::mpsc;

// These get reported like so on Linux:
// FL STUDIO FIRE:FL STUDIO FIRE MIDI 1 32:0
// FL STUDIO FIRE:FL STUDIO FIRE MIDI 1 36:0
//
// And on Windows:
// FL STUDIO FIRE
// FL STUDIO FIRE
pub const MIDI_INPUT_PORT_PREFIX: &'static str = "FL STUDIO FIRE"; //:FL STUDIO FIRE MIDI 1 ";
pub const MIDI_OUTPUT_PORT_PREFIX: &'static str = "FL STUDIO FIRE"; //:FL STUDIO FIRE MIDI 1 ";

struct ConnectedController {
    in_conn: MidiInputConnection<()>,
    out_conn: MidiOutputConnection,
}

enum ControllerState {
    Disconnected,
    Connected(ConnectedController),
}

/// Controller Buttons, Left-to-right, Top-to-bottom, first non-shifted label
/// associated with the button except for the grid row buttons.
#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum ControllerButton {
    // ## Top Row
    // "Channel"/"Mixer"/"User 1"/"User 2"
    Channel,
    PatternUp,
    PatternDown,
    Browser,
    GridLeft,
    GridRight,
    // ## The Grid Row Buttons ("Mute"/"Solo")
    Row1,
    Row2,
    Row3,
    Row4,
    // ## Bottom Row
    Step,
    Note,
    Drum,
    Perform,
    Shift,
    Alt,
    Pattern,
    Play,
    Stop,
    Record,
    // XXX this one shouldn't end up used...
    Mystery,
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum ControllerKnob {
    Volume,
    Pan,
    Filter,
    Resonance,
    Select,
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum ButtonState {
    Down,
    Up
}

#[derive(Copy, Clone, Debug, Hash, PartialEq, Eq)]
pub enum ControllerEvent {
    ControlButton(ControllerButton, ButtonState),
    KnobTurn(ControllerKnob, u8),
    KnobTouch(ControllerKnob, ButtonState),
    /// A grid button changed state.  (index, row0, col0, velocity)
    GridButton(u8, u8, u8, ButtonState, u8),
}

impl ControllerEvent {
    pub fn from_midi(msg: &[u8]) -> Option<Self> {
        match msg.len() {
            3 => match (msg[0], msg[1], msg[2]) {
                // ## Knobs!
                (ud @ (0x90 | 0x80 | 0xb0),  kn @ 0x10..=0x19, value) => {
                    let knob = match kn {
                        0x10 => ControllerKnob::Volume,
                        0x11 => ControllerKnob::Pan,
                        0x12 => ControllerKnob::Filter,
                        0x13 => ControllerKnob::Resonance,
                        0x19 => ControllerKnob::Select,
                        _ => unreachable!(),
                    };
                    match ud {
                        0x90 => Some(ControllerEvent::KnobTouch(knob, ButtonState::Down)),
                        0x80 => Some(ControllerEvent::KnobTouch(knob, ButtonState::Up)),
                        0xb0 => Some(ControllerEvent::KnobTurn(knob, value)),
                        _ => unreachable!(),
                    }
                },
                // ## Labeled Buttons!
                (ud @ (0x90 | 0x80),  btn @ 0x1a..=0x35, _) => {
                    let state = match ud {
                        0x90 => ButtonState::Down,
                        0x80 => ButtonState::Up,
                        _ => unreachable!(),
                    };
                    let button = match btn {
                        0x1a => ControllerButton::Channel,
                        0x1f => ControllerButton::PatternUp,
                        0x20 => ControllerButton::PatternDown,
                        0x21 => ControllerButton::Browser,
                        0x22 => ControllerButton::GridLeft,
                        0x23 => ControllerButton::GridRight,
                        0x24 => ControllerButton::Row1,
                        0x25 => ControllerButton::Row2,
                        0x26 => ControllerButton::Row3,
                        0x27 => ControllerButton::Step,
                        0x2d => ControllerButton::Note,
                        0x2e => ControllerButton::Drum,
                        0x2f => ControllerButton::Perform,
                        0x30 => ControllerButton::Shift,
                        0x31 => ControllerButton::Alt,
                        0x32 => ControllerButton::Pattern,
                        0x33 => ControllerButton::Play,
                        0x34 => ControllerButton::Stop,
                        0x35 => ControllerButton::Record,
                        _ => ControllerButton::Mystery,
                    };
                    Some(ControllerEvent::ControlButton(button, state))
                },
                // ## The grid (pads)!
                (ud @ (0x90 | 0x80),  btn @ 0x36..=0x75, vel) => {
                    let state = match ud {
                        0x90 => ButtonState::Down,
                        0x80 => ButtonState::Up,
                        _ => unreachable!(),
                    };
                    let index = btn - 0x36;
                    let row0 = index / 16;
                    let col0 = index % 16;
                    Some(ControllerEvent::GridButton(index, row0, col0, state, vel))
                },
                _ => None,
            },
            _ => None,
        }
    }
}

pub struct FireController {
    /// Identifier for the controller.  Ideally this would be the serial number
    /// of the device extracted via sysex or the USB path to the device.  Right
    /// now it's just a one-up.
    id: u32,
    state: ControllerState,
    event_rx: Option<mpsc::Receiver<ControllerEvent>>,

    // 7 header bytes + (4 bytes per grid led * 64 leds) + 1 end byte.
    led_msg_buf: [u8; 7 + 4 * 64 + 1],
}


impl FireController {
    /// Finds all Fire controllers on the system and returns them in a vector.
    pub fn attach_to_all() -> Vec<FireController> {
        let mut controllers: Vec<FireController> = vec![];

        // We iterate over all input ports and for those that match the prefix,
        // we find the exact matching output port.  The ownership model is that
        // calling connect() on a MidiInput consumes (moves) it, so we do a
        // pass to figure out the port names we want, and then a pass that
        // creates MidiInput and MidiOutput instances to connect to that
        // specific instance.

        let walk_in = MidiInput::new("Fire-Walk").unwrap();
        // Accumulate the list of ports completely first so there's no overlap
        // of MidiInput lifetimes.
        let desired_names : Vec<String> = walk_in.ports().into_iter().filter_map(|p| {
            let name = walk_in.port_name(&p).unwrap();
            if name.starts_with(MIDI_INPUT_PORT_PREFIX) {
                Some(name)
            } else {
                None
            }
        }).collect();

        for (i, desired_name) in desired_names.into_iter().enumerate() {
            let midi_in = MidiInput::new("Fire-Walk").unwrap();
            let midi_out = MidiOutput::new("Fire").unwrap();

            let (mut tx, mut rx) = mpsc::channel::<ControllerEvent>(100);

            let in_port = midi_in.ports().into_iter().find_map(|p| {
                if midi_in.port_name(&p).unwrap() == desired_name {
                    Some(p)
                } else {
                    None
                }
            }).unwrap();
            let in_conn = midi_in.connect(
                &in_port, "fire-in", move |_stamp, msg, _| {
                    if let Some(event) = ControllerEvent::from_midi(msg) {
                        tx.try_send(event).expect("Send exploded");
                    }
                }, ()).unwrap();

            // The out port should have the same name as the in name.
            let out_port = midi_out.ports().into_iter().find_map(|p| {
                if midi_out.port_name(&p).unwrap() == desired_name {
                    Some(p)
                } else {
                    None
                }
            }).unwrap();
            let out_conn = midi_out.connect(&out_port, "fire-out").unwrap();

            let mut controller = FireController {
                id: i as u32,
                state: ControllerState::Connected(ConnectedController {
                    in_conn,
                    out_conn,
                }),
                event_rx: Some(rx),
                led_msg_buf: [0; 264],
            };
            controller.init();
            controllers.push(controller);
        }

        controllers
    }

    /// Initializes any pre-allocated buffers.
    fn init(&mut self) {
        let len: u16 = 4 * 64;
        self.led_msg_buf[0..7].copy_from_slice(
            &[0xf0, 0x47, 0x7f, 0x43, 0x65, ((len >> 7)&0x7f) as u8, (len&0x7f) as u8]);

        // The first byte of each 4-byte tuple is the index of the button to
        // update.
        for i in 0..64 {
            self.led_msg_buf[7 + i * 4] = i as u8;
        }
        self.led_msg_buf[self.led_msg_buf.len() - 1] = 0xf7;
    }


    /// Do a basic 4x4 color cube cut into 4 slices.
    pub fn set_color_cube(&mut self) {
        for i in 0..64 {
            let x: u8 = i % 4;
            let y: u8 = i / 16;
            let z: u8 = (i % 16) / 4;
            self.led_msg_buf[7 + (i as usize) * 4 + 1] = min(0x7f, x * 0x20);
            self.led_msg_buf[7 + (i as usize) * 4 + 2] = min(0x7f, y * 0x20);
            self.led_msg_buf[7 + (i as usize) * 4 + 3] = min(0x7f, z * 0x20);
        }
    }

    pub fn set_led(&mut self, i: u8, r: u8, g: u8, b: u8) {
        self.led_msg_buf[7 + (i as usize) * 4 + 1] = min(0x7f, r);
        self.led_msg_buf[7 + (i as usize) * 4 + 2] = min(0x7f, g);
        self.led_msg_buf[7 + (i as usize) * 4 + 3] = min(0x7f, b);
    }

    pub fn update_leds(&mut self) {
        if let ControllerState::Connected(cs) = &mut self.state {
            cs.out_conn.send(&self.led_msg_buf).unwrap();
        }
    }
}

impl Hash for FireController {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Eq for FireController {}

impl PartialEq for FireController {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

#[tokio::main]
async fn main() {
    let mut controllers = FireController::attach_to_all();

    let mut map = StreamMap::new();

    for (i, c) in controllers.iter_mut().enumerate() {
        c.set_color_cube();
        c.update_leds();

        if let Some(rx) = c.event_rx.take() {
            map.insert(i, rx);
        }
    }

    while let Some((i, evt)) = map.next().await {
        let c = controllers.get_mut(i).unwrap();
        match evt {
            ControllerEvent::GridButton(idx, _, _, ButtonState::Down, _) => {
                c.set_led(idx, 0x7f, 0x7f, 0x7f);
                c.update_leds();
            },
            ControllerEvent::GridButton(idx, _, _, ButtonState::Up, _) => {
                c.set_led(idx, 0, 0, 0);
                c.update_leds();
            },
            _ => ()
        }
    }

    ()
}
