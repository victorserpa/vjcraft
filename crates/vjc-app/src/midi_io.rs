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
    /// Raw message from a device (by port name) and the instant the callback received it.
    Msg(String, Vec<u8>, Instant),
    /// A device appeared (its LEDs need their state again).
    Connected(String),
    /// One direction of a device went away (unplugged); it is reconnected by name when it
    /// returns.
    Disconnected(String, Dir),
}

/// Which side of a MIDI device is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Dir {
    Input,
    Output,
}

impl Dir {
    pub fn label(self) -> &'static str {
        match self {
            Dir::Input => "input",
            Dir::Output => "output",
        }
    }
}

/// Disconnect notices per direction. A direction that vanished and came back between two
/// polls (it is in its own `fresh`) is present now, so it is not reported.
fn gone_events(
    fresh_in: &[String],
    gone_in: Vec<String>,
    fresh_out: &[String],
    gone_out: Vec<String>,
) -> Vec<(String, Dir)> {
    let mut v: Vec<(String, Dir)> = gone_in
        .into_iter()
        .filter(|g| !fresh_in.contains(g))
        .map(|g| (g, Dir::Input))
        .chain(
            gone_out
                .into_iter()
                .filter(|g| !fresh_out.contains(g))
                .map(|g| (g, Dir::Output)),
        )
        .collect();
    v.sort();
    v.dedup();
    v
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

    /// No thread, no devices (UI tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn idle() -> Midi {
        let (_tx, rx) = channel();
        let (out, _out_rx) = channel();
        Midi { rx, out }
    }

    /// Idle, with the sender that feeds `rx` (UI tests inject messages).
    #[cfg(test)]
    pub fn idle_fed() -> (Midi, Sender<In>) {
        let (tx, rx) = channel();
        let (out, _out_rx) = channel();
        (Midi { rx, out }, tx)
    }

    pub fn send(&self, device: &str, bytes: [u8; 3]) {
        let _ = self.out.send((device.to_owned(), bytes));
    }
}

/// Names of the MIDI input ports now present.
pub fn input_ports() -> Vec<String> {
    let Ok(probe) = MidiInput::new(CLIENT) else {
        return vec![];
    };
    probe
        .ports()
        .iter()
        .filter_map(|p| probe.port_name(p).ok())
        .collect()
}

/// Names of the MIDI output ports now present.
pub fn output_ports() -> Vec<String> {
    let Ok(probe) = MidiOutput::new(CLIENT) else {
        return vec![];
    };
    probe
        .ports()
        .iter()
        .filter_map(|p| probe.port_name(p).ok())
        .collect()
}

fn run(tx: Sender<In>, out_rx: Receiver<(String, [u8; 3])>) {
    let mut inputs: HashMap<String, MidiInputConnection<()>> = HashMap::new();
    let mut outputs: HashMap<String, midir::MidiOutputConnection> = HashMap::new();
    let mut last = Instant::now() - POLL;
    loop {
        if last.elapsed() >= POLL {
            last = Instant::now();
            let (mut fresh, gone_in) = sync_inputs(&tx, &mut inputs);
            let (fresh_out, gone_out) = sync_outputs(&mut outputs);
            let gone = gone_events(&fresh, gone_in, &fresh_out, gone_out);
            fresh.extend(fresh_out);
            fresh.sort();
            fresh.dedup();
            for (name, dir) in gone {
                let _ = tx.send(In::Disconnected(name, dir));
            }
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

/// MIDI callback body: stamps the message on arrival, not when the UI drains it.
fn on_message(tx: &Sender<In>, device: &str, bytes: &[u8]) {
    let _ = tx.send(In::Msg(device.to_owned(), bytes.to_vec(), Instant::now()));
    crate::wake::now();
}

/// Brings the open connections in line with the ports now present: drops those whose port
/// vanished (unplugged), opens new ones through `connect` (a failure is skipped and retried
/// at the next poll, so one bad device never blocks the others). Returns
/// `(newly connected, gone)` names. A device that comes back under the same name is simply
/// "new" again, so replug reconnects by name.
fn reconcile<C>(
    open: &mut HashMap<String, C>,
    present: &[String],
    mut connect: impl FnMut(&str) -> Option<C>,
) -> (Vec<String>, Vec<String>) {
    let gone: Vec<String> = open
        .keys()
        .filter(|n| !present.contains(n))
        .cloned()
        .collect();
    for n in &gone {
        open.remove(n);
    }
    let mut fresh = vec![];
    for name in present {
        if open.contains_key(name) {
            continue;
        }
        if let Some(c) = connect(name) {
            open.insert(name.clone(), c);
            fresh.push(name.clone());
        }
    }
    (fresh, gone)
}

/// Runs one poll step; a panic inside the backend (a driver misbehaving on unplug) is
/// contained: the connections are dropped and the next poll starts clean.
fn guarded<C, R: Default>(
    open: &mut HashMap<String, C>,
    f: impl FnOnce(&mut HashMap<String, C>) -> R,
) -> R {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(open))) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("midi: backend panicked while polling; reconnecting");
            open.clear();
            R::default()
        }
    }
}

/// Opens new input ports; returns the names newly connected and the ones that went away.
fn sync_inputs(
    tx: &Sender<In>,
    open: &mut HashMap<String, MidiInputConnection<()>>,
) -> (Vec<String>, Vec<String>) {
    guarded(open, |open| {
        let Ok(probe) = MidiInput::new(CLIENT) else {
            return (vec![], vec![]);
        };
        let ports: Vec<(String, _)> = probe
            .ports()
            .into_iter()
            .filter_map(|p| Some((probe.port_name(&p).ok()?, p)))
            .collect();
        let names: Vec<String> = ports.iter().map(|(n, _)| n.clone()).collect();
        reconcile(open, &names, |name| {
            let port = &ports.iter().find(|(n, _)| n == name)?.1;
            let input = MidiInput::new(CLIENT).ok()?;
            let sender = tx.clone();
            let dev = name.to_owned();
            input
                .connect(
                    port,
                    "vjcraft-in",
                    move |_, bytes, _| on_message(&sender, &dev, bytes),
                    (),
                )
                .map_err(|e| eprintln!("midi: input {name}: {e}"))
                .ok()
        })
    })
}

/// Opens new output ports (including ones dropped after a failed send);
/// returns their names so LED state is resent, and the ones that went away.
fn sync_outputs(
    open: &mut HashMap<String, midir::MidiOutputConnection>,
) -> (Vec<String>, Vec<String>) {
    guarded(open, |open| {
        let Ok(probe) = MidiOutput::new(CLIENT) else {
            return (vec![], vec![]);
        };
        let ports: Vec<(String, _)> = probe
            .ports()
            .into_iter()
            .filter_map(|p| Some((probe.port_name(&p).ok()?, p)))
            .collect();
        let names: Vec<String> = ports.iter().map(|(n, _)| n.clone()).collect();
        reconcile(open, &names, |name| {
            let port = &ports.iter().find(|(n, _)| n == name)?.1;
            let output = MidiOutput::new(CLIENT).ok()?;
            output
                .connect(port, "vjcraft-out")
                .map_err(|e| eprintln!("midi: output {name}: {e}"))
                .ok()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lister(steps: Vec<Vec<&'static str>>) -> impl FnMut() -> Vec<String> {
        let mut i = 0;
        move || {
            let v = steps[i.min(steps.len() - 1)]
                .iter()
                .map(|s| s.to_string())
                .collect();
            i += 1;
            v
        }
    }

    #[test]
    fn unplug_drops_the_device_and_replug_reconnects_by_name() {
        let mut list = lister(vec![vec!["Pad", "Keys"], vec!["Keys"], vec!["Pad", "Keys"]]);
        let mut open: HashMap<String, u32> = HashMap::new();
        let mut opened = 0;
        let mut connect = |_: &str| {
            opened += 1;
            Some(opened)
        };
        let (fresh, gone) = reconcile(&mut open, &list(), &mut connect);
        assert_eq!((fresh.len(), gone.len()), (2, 0));
        // Pad unplugged: no panic, only Keys stays, Pad is reported gone.
        let (fresh, gone) = reconcile(&mut open, &list(), &mut connect);
        assert!(fresh.is_empty());
        assert_eq!(gone, vec!["Pad".to_string()]);
        assert!(open.contains_key("Keys") && !open.contains_key("Pad"));
        // Pad replugged: reconnected under the same name, Keys untouched.
        let (fresh, gone) = reconcile(&mut open, &list(), &mut connect);
        assert_eq!(fresh, vec!["Pad".to_string()]);
        assert!(gone.is_empty());
        assert_eq!(opened, 3, "Keys was not reopened");
    }

    #[test]
    fn a_device_that_fails_to_open_is_retried_and_does_not_block_others() {
        let mut open: HashMap<String, u32> = HashMap::new();
        let present = vec!["Bad".to_string(), "Good".to_string()];
        let (fresh, _) = reconcile(&mut open, &present, |n| (n == "Good").then_some(1));
        assert_eq!(fresh, vec!["Good".to_string()]);
        // Negative control: without retry the device would stay out forever.
        let (fresh, _) = reconcile(&mut open, &present, |_| Some(2));
        assert_eq!(fresh, vec!["Bad".to_string()]);
    }

    #[test]
    fn a_panicking_backend_is_contained_and_the_next_poll_recovers() {
        let mut open: HashMap<String, u32> = HashMap::from([("Pad".to_string(), 1)]);
        let r: (Vec<String>, Vec<String>) = guarded(&mut open, |_| panic!("driver blew up"));
        assert_eq!(r, (vec![], vec![]));
        assert!(open.is_empty(), "stale connections are dropped");
        let (fresh, _) = guarded(&mut open, |o| {
            reconcile(o, &["Pad".to_string()], |_| Some(1))
        });
        assert_eq!(fresh, vec!["Pad".to_string()]);
    }

    #[test]
    fn message_timestamp_is_taken_in_the_callback() {
        let (tx, rx) = channel();
        let before = Instant::now();
        on_message(&tx, "dev", &[0xF1, 0x00]);
        let after = Instant::now();
        std::thread::sleep(Duration::from_millis(30));
        let Ok(In::Msg(d, b, at)) = rx.try_recv() else {
            panic!("no message");
        };
        assert_eq!((d.as_str(), b), ("dev", vec![0xF1, 0x00]));
        assert!(
            at >= before && at <= after,
            "stamped at receive, not at drain"
        );
    }

    #[test]
    fn disconnect_is_reported_per_direction() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // Only the output of "Pad" vanished: the input side is not reported.
        let ev = gone_events(&[], vec![], &[], s(&["Pad"]));
        assert_eq!(ev, vec![("Pad".to_string(), Dir::Output)]);
        let ev = gone_events(&[], s(&["Pad"]), &[], s(&["Pad"]));
        assert_eq!(ev.len(), 2);
        // Negative control: an output back within the same poll is not reported, while the
        // input that really left still is.
        let ev = gone_events(&[], s(&["Pad"]), &s(&["Pad"]), s(&["Pad"]));
        assert_eq!(ev, vec![("Pad".to_string(), Dir::Input)]);
    }
}
