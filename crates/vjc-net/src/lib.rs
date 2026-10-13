//! Network video for VJCraft: Syphon (macOS, Metal API, vendored framework) and NDI
//! (libndi loaded at runtime, never linked or bundled). Everything degrades to an error
//! string when the backend is missing; nothing here panics because of a missing runtime.
//!
//! Frames are RGBA8, tightly packed, top row first.

pub mod convert;
#[cfg(feature = "ndi")]
pub mod ndi;
#[cfg(all(feature = "syphon", target_os = "macos"))]
pub mod syphon;

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Syphon,
    Ndi,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Syphon => "Syphon",
            Protocol::Ndi => "NDI",
        }
    }
}

/// A discoverable server (Syphon) or source (NDI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetSource {
    pub protocol: Protocol,
    /// What the user sees and what a clip stores (`Source::Network::name`).
    pub label: String,
    /// Application name (Syphon) or machine (NDI), may be empty.
    pub app: String,
    /// Server name as published.
    pub name: String,
    /// Backend identifier (Syphon UUID; the NDI name).
    pub id: String,
}

/// Picks the entry a stored clip name refers to: exact label, then case-insensitive label,
/// then a unique match on the bare server name.
pub fn resolve<'a>(sources: &'a [NetSource], wanted: &str) -> Option<&'a NetSource> {
    if let Some(s) = sources.iter().find(|s| s.label == wanted) {
        return Some(s);
    }
    if let Some(s) = sources
        .iter()
        .find(|s| s.label.eq_ignore_ascii_case(wanted))
    {
        return Some(s);
    }
    let mut by_name = sources.iter().filter(|s| s.name == wanted);
    match (by_name.next(), by_name.next()) {
        (Some(s), None) => Some(s),
        _ => None,
    }
}

/// Lists what is currently available. May block up to about a second for NDI.
pub fn discover(protocol: Protocol) -> Result<Vec<NetSource>, String> {
    match protocol {
        #[cfg(all(feature = "syphon", target_os = "macos"))]
        Protocol::Syphon => Ok(syphon::list()),
        #[cfg(feature = "ndi")]
        Protocol::Ndi => ndi::list(),
        #[allow(unreachable_patterns)]
        p => Err(unavailable(p)),
    }
}

pub fn unavailable(p: Protocol) -> String {
    format!("{} is not available in this build", p.label())
}

/// Whether the backend can work at all here (NDI: libndi found). Cheap after first call.
pub fn available(protocol: Protocol) -> Result<(), String> {
    match protocol {
        #[cfg(all(feature = "syphon", target_os = "macos"))]
        Protocol::Syphon => Ok(()),
        #[cfg(feature = "ndi")]
        Protocol::Ndi => ndi::lib().map(|_| ()),
        #[allow(unreachable_patterns)]
        p => Err(unavailable(p)),
    }
}

/// Publishes frames under a name.
pub enum Sender {
    #[cfg(all(feature = "syphon", target_os = "macos"))]
    Syphon(syphon::Server),
    #[cfg(feature = "ndi")]
    Ndi(ndi::Sender),
}

impl Sender {
    pub fn open(protocol: Protocol, name: &str) -> Result<Sender, String> {
        match protocol {
            #[cfg(all(feature = "syphon", target_os = "macos"))]
            Protocol::Syphon => syphon::Server::new(name).map(Sender::Syphon),
            #[cfg(feature = "ndi")]
            Protocol::Ndi => ndi::Sender::new(name).map(Sender::Ndi),
            #[allow(unreachable_patterns)]
            p => Err(unavailable(p)),
        }
    }

    /// False while nobody is listening, so the caller can skip producing frames.
    pub fn wants_frames(&self) -> bool {
        match self {
            #[cfg(all(feature = "syphon", target_os = "macos"))]
            Sender::Syphon(s) => s.has_clients(),
            #[cfg(feature = "ndi")]
            Sender::Ndi(s) => s.wants_frames(),
            #[allow(unreachable_patterns)]
            _ => true,
        }
    }

    /// Publishes one RGBA8 frame with `stride` bytes per row. `fps` is the project rate
    /// (declared to NDI receivers; Syphon ignores it).
    pub fn send(
        &mut self,
        w: u32,
        h: u32,
        stride: u32,
        rgba: &[u8],
        fps: f32,
    ) -> Result<(), String> {
        match self {
            #[cfg(all(feature = "syphon", target_os = "macos"))]
            Sender::Syphon(s) => s.publish(w, h, stride, rgba),
            #[cfg(feature = "ndi")]
            Sender::Ndi(s) => s.send(w, h, stride, rgba, fps),
            #[allow(unreachable_patterns)]
            _ => {
                let _ = fps;
                Err("no backend".into())
            }
        }
    }
}

/// Receives frames from the source called `name` until `sink` returns false. Retries
/// while the source is not (yet) available. Blocks; run it on its own thread.
pub fn receive(
    protocol: Protocol,
    name: &str,
    sink: &mut dyn FnMut(u32, u32, Vec<u8>) -> bool,
) -> Result<(), String> {
    match protocol {
        #[cfg(all(feature = "syphon", target_os = "macos"))]
        Protocol::Syphon => syphon::receive(name, sink),
        #[cfg(feature = "ndi")]
        Protocol::Ndi => ndi::receive(name, sink),
        #[allow(unreachable_patterns)]
        p => Err(unavailable(p)),
    }
}

/// Reconnect interval.
pub(crate) const RETRY: Duration = Duration::from_millis(1000);
