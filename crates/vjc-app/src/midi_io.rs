//! MIDI device I/O on its own thread: every input port is opened and polled for hot-plug
//! every 1.5 s; output ports are opened by device name for LED feedback. The UI only touches
//! two channels (`try_recv` / `send`), so it never blocks, and with no devices (or no MIDI
//! backend) the thread just idles.

use midir::{MidiInput, MidiInputConnection, MidiOutput};
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(1500);
const CLIENT: &str = "VJCraft";

pub enum In {
    /// Raw message from a device (by port name).
    Msg(String, Vec<u8>),
    /// A device appeared (its LEDs need their state again).
    Connected(String),
}

pub struct Midi {
    pub rx: Receiver<In>,
    out: Sender<(String, [u8; 3])>,
}

impl Midi {
    pub fn spawn() -> Midi {
        let (tx, rx) = channel();
        let (out, out_rx) = channel::<(String, [u8; 3])>();
        let spawned = std::thread::Builder::new()
            .name("midi".into())
            .spawn(move || run(tx, out_rx));
        if let Err(e) = spawned {
            eprintln!("midi: thread not started: {e}");
        }
        Midi { rx, out }
    }

    pub fn send(&self, device: &str, bytes: [u8; 3]) {
        let _ = self.out.send((device.to_owned(), bytes));
    }
}

fn run(tx: Sender<In>, out_rx: Receiver<(String, [u8; 3])>) {
    let mut inputs: HashMap<String, MidiInputConnection<()>> = HashMap::new();
    let mut outputs: HashMap<String, midir::MidiOutputConnection> = HashMap::new();
    let mut last = Instant::now() - POLL;
    loop {
        if last.elapsed() >= POLL {
            last = Instant::now();
            let mut fresh = sync_inputs(&tx, &mut inputs);
            fresh.extend(sync_outputs(&mut outputs));
            fresh.sort();
            fresh.dedup();
            // Announce only after outputs are open, so the UI's LED resend
            // is not dropped for a port that is not connected yet.
            for name in fresh {
                let _ = tx.send(In::Connected(name));
            }
        }
        match out_rx.recv_timeout(Duration::from_millis(100)) {
            Ok((device, bytes)) => {
                if let Some(c) = outputs.get_mut(&device)
                    && c.send(&bytes).is_err()
                {
                    outputs.remove(&device);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
    }
}

/// Opens new input ports; returns the names newly connected.
fn sync_inputs(
    tx: &Sender<In>,
    open: &mut HashMap<String, MidiInputConnection<()>>,
) -> Vec<String> {
    let mut fresh = vec![];
    let Ok(probe) = MidiInput::new(CLIENT) else {
        return fresh;
    };
    let ports: Vec<(String, _)> = probe
        .ports()
        .into_iter()
        .filter_map(|p| Some((probe.port_name(&p).ok()?, p)))
        .collect();
    open.retain(|name, _| ports.iter().any(|(n, _)| n == name));
    for (name, port) in ports {
        if open.contains_key(&name) {
            continue;
        }
        let Ok(input) = MidiInput::new(CLIENT) else {
            continue;
        };
        let sender = tx.clone();
        let dev = name.clone();
        match input.connect(
            &port,
            "vjcraft-in",
            move |_, bytes, _| {
                let _ = sender.send(In::Msg(dev.clone(), bytes.to_vec()));
            },
            (),
        ) {
            Ok(conn) => {
                open.insert(name.clone(), conn);
                fresh.push(name);
            }
            Err(e) => eprintln!("midi: input {name}: {e}"),
        }
    }
    fresh
}

/// Opens new output ports (including ones dropped after a failed send);
/// returns their names so LED state is resent.
fn sync_outputs(open: &mut HashMap<String, midir::MidiOutputConnection>) -> Vec<String> {
    let mut fresh = vec![];
    let Ok(probe) = MidiOutput::new(CLIENT) else {
        return fresh;
    };
    let ports: Vec<(String, _)> = probe
        .ports()
        .into_iter()
        .filter_map(|p| Some((probe.port_name(&p).ok()?, p)))
        .collect();
    open.retain(|name, _| ports.iter().any(|(n, _)| n == name));
    for (name, port) in ports {
        if open.contains_key(&name) {
            continue;
        }
        let Ok(output) = MidiOutput::new(CLIENT) else {
            continue;
        };
        match output.connect(&port, "vjcraft-out") {
            Ok(conn) => {
                open.insert(name.clone(), conn);
                fresh.push(name);
            }
            Err(e) => eprintln!("midi: output {name}: {e}"),
        }
    }
    fresh
}
