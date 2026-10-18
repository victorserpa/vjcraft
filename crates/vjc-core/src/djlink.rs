//! DJ player sync, pure: decode of the publicly documented community notes on the
//! players' LAN protocol (beat broadcasts and status), a tracker of what each player
//! says, and a follower that steers the master `Clock`. No sockets here.
//!
//! Clean room: written from protocol descriptions only, no vendor SDK or code. The
//! listener is passive (it never announces a device), so beat packets give tempo, bar
//! position and phase; status packets (master flag, beat counter) are used when they
//! reach us. Byte offsets come from those notes and are unverified on real hardware.

use crate::clock::Clock;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// UDP port of beat broadcasts.
pub const BEAT_PORT: u16 = 50001;
/// UDP port of status packets.
pub const STATUS_PORT: u16 = 50002;
/// Pitch value meaning 0 % change.
pub const NORMAL_PITCH: u32 = 0x10_0000;
/// A player silent for longer than this is gone.
pub const STALE: Duration = Duration::from_secs(2);

/// Track tempo is in hundredths of BPM; the pitch scales it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    pub player: u8,
    /// Tempo of the track at normal pitch.
    pub bpm: f64,
    pub pitch: u32,
    /// 1..=4.
    pub beat_in_bar: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Status {
    pub player: u8,
    pub master: bool,
    pub playing: bool,
    pub bpm: f64,
    pub pitch: u32,
    /// Beats since the start of the track, None when unknown.
    pub beat_count: Option<u32>,
    pub beat_in_bar: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Packet {
    Beat(Beat),
    Status(Status),
}

/// Tempo actually heard: track tempo scaled by pitch.
pub fn effective_bpm(bpm: f64, pitch: u32) -> f64 {
    bpm * f64::from(pitch) / f64::from(NORMAL_PITCH)
}

const MAGIC: &[u8; 10] = b"Qspt1WmJOL";
const KIND_BEAT: u8 = 0x28;
const KIND_STATUS: u8 = 0x0a;
const BEAT_LEN: usize = 0x60;
const STATUS_LEN: usize = 0xd4;
const NO_BPM: u16 = 0xffff;
const NO_COUNT: u32 = 0xffff_ffff;
const FLAG_PLAY: u8 = 0x40;
const FLAG_MASTER: u8 = 0x20;
const SIM_NAME: &[u8] = b"VJCraft-sim";

fn be16(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}

fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Decodes one datagram; anything malformed, truncated or of another kind is None.
pub fn decode(b: &[u8]) -> Option<Packet> {
    if b.len() < 0x0b || &b[..10] != MAGIC {
        return None;
    }
    match b[0x0a] {
        KIND_BEAT if b.len() >= BEAT_LEN => {
            let beat_in_bar = b[0x5c];
            if !(1..=4).contains(&beat_in_bar) {
                return None;
            }
            Some(Packet::Beat(Beat {
                player: b[0x21],
                bpm: f64::from(be16(b, 0x5a)) / 100.0,
                pitch: be32(b, 0x54),
                beat_in_bar,
            }))
        }
        KIND_STATUS if b.len() >= STATUS_LEN => {
            let beat_in_bar = b[0xa6];
            if beat_in_bar > 4 {
                return None;
            }
            let raw_bpm = be16(b, 0x92);
            let count = be32(b, 0xa0);
            Some(Packet::Status(Status {
                player: b[0x21],
                master: b[0x89] & FLAG_MASTER != 0,
                playing: b[0x89] & FLAG_PLAY != 0,
                bpm: if raw_bpm == NO_BPM {
                    0.0
                } else {
                    f64::from(raw_bpm) / 100.0
                },
                pitch: be32(b, 0x8c),
                beat_count: (count != NO_COUNT).then_some(count),
                beat_in_bar,
            }))
        }
        _ => None,
    }
}

fn header(kind: u8, player: u8, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    v[..10].copy_from_slice(MAGIC);
    v[0x0a] = kind;
    v[0x0b..0x0b + SIM_NAME.len()].copy_from_slice(SIM_NAME);
    v[0x1f] = 1;
    v[0x21] = player;
    v[0x22..0x24].copy_from_slice(&((len - 0x24) as u16).to_be_bytes());
    v
}

/// Builds a beat packet in the documented layout (loopback simulator and tests).
pub fn encode_beat(b: &Beat) -> Vec<u8> {
    let mut v = header(KIND_BEAT, b.player, BEAT_LEN);
    v[0x54..0x58].copy_from_slice(&b.pitch.to_be_bytes());
    v[0x5a..0x5c].copy_from_slice(&((b.bpm * 100.0).round() as u16).to_be_bytes());
    v[0x5c] = b.beat_in_bar;
    v[0x5f] = b.player;
    v
}

/// Builds a status packet in the documented layout (loopback simulator and tests).
pub fn encode_status(s: &Status) -> Vec<u8> {
    let mut v = header(KIND_STATUS, s.player, STATUS_LEN);
    v[0x89] = (if s.playing { FLAG_PLAY } else { 0 }) | (if s.master { FLAG_MASTER } else { 0 });
    v[0x8c..0x90].copy_from_slice(&s.pitch.to_be_bytes());
    let bpm = if s.bpm > 0.0 {
        (s.bpm * 100.0).round() as u16
    } else {
        NO_BPM
    };
    v[0x92..0x94].copy_from_slice(&bpm.to_be_bytes());
    v[0xa0..0xa4].copy_from_slice(&s.beat_count.unwrap_or(NO_COUNT).to_be_bytes());
    v[0xa6] = s.beat_in_bar;
    v
}

/// The deck the clock follows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Lead {
    pub player: u8,
    /// Tempo heard (pitch applied).
    pub bpm: f64,
    pub beat_in_bar: u8,
    /// When the last beat of this player arrived.
    pub beat_at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Deck {
    seen: Instant,
    master: bool,
    beat: Option<Lead>,
    beat_count: Option<u32>,
}

#[derive(Debug, Default)]
pub struct Tracker {
    decks: BTreeMap<u8, Deck>,
}

impl Tracker {
    fn deck(&mut self, player: u8, at: Instant) -> &mut Deck {
        let d = self.decks.entry(player).or_insert(Deck {
            seen: at,
            master: false,
            beat: None,
            beat_count: None,
        });
        d.seen = at;
        d
    }

    pub fn feed(&mut self, p: Packet, at: Instant) {
        match p {
            Packet::Beat(b) => {
                self.deck(b.player, at).beat = Some(Lead {
                    player: b.player,
                    bpm: effective_bpm(b.bpm, b.pitch),
                    beat_in_bar: b.beat_in_bar,
                    beat_at: at,
                });
            }
            Packet::Status(s) => {
                if s.master {
                    for d in self.decks.values_mut() {
                        d.master = false;
                    }
                }
                let d = self.deck(s.player, at);
                d.master = s.master;
                d.beat_count = s.beat_count;
            }
        }
    }

    fn live(d: &Deck, now: Instant) -> bool {
        now.saturating_duration_since(d.seen) <= STALE
    }

    /// `follow` 0 = the master player (flagged, else the lowest-numbered live one).
    pub fn lead(&self, follow: u8, now: Instant) -> Option<Lead> {
        let fresh = |(_, d): &(&u8, &Deck)| {
            Self::live(d, now)
                && d.beat
                    .is_some_and(|b| now.saturating_duration_since(b.beat_at) <= STALE)
        };
        let pick = if follow != 0 {
            self.decks.get_key_value(&follow).filter(fresh)
        } else {
            let mut live = self.decks.iter().filter(fresh);
            let first = live.next();
            self.decks
                .iter()
                .filter(fresh)
                .find(|(_, d)| d.master)
                .or(first)
        };
        pick.and_then(|(_, d)| d.beat)
    }

    /// Live players, ascending.
    pub fn players(&self, now: Instant) -> Vec<u8> {
        self.decks
            .iter()
            .filter(|(_, d)| Self::live(d, now))
            .map(|(&n, _)| n)
            .collect()
    }

    pub fn beat_count(&self, player: u8) -> Option<u32> {
        self.decks.get(&player).and_then(|d| d.beat_count)
    }
}

/// What the clock area shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyncState {
    Off,
    Waiting,
    Locked { player: u8, bpm: f64 },
}

#[derive(Debug)]
pub struct Follower {
    pub state: SyncState,
}

impl Default for Follower {
    fn default() -> Self {
        Follower {
            state: SyncState::Off,
        }
    }
}

impl Follower {
    /// Once per frame. While a player is heard, tempo is taken from it and the phase is
    /// steered so the bar position matches its beats. When it goes silent the clock
    /// keeps its last tempo and runs free.
    pub fn update(&mut self, t: &Tracker, on: bool, follow: u8, c: &mut Clock, now: Instant) {
        if !on {
            self.state = SyncState::Off;
            return;
        }
        let Some(l) = t.lead(follow, now) else {
            self.state = SyncState::Waiting;
            return;
        };
        let bpm = l.bpm.clamp(crate::clock::MIN_BPM, crate::clock::MAX_BPM);
        if (c.bpm() - bpm).abs() > 1e-6 {
            c.set_bpm(bpm, now);
        }
        let since = now.saturating_duration_since(l.beat_at).as_secs_f64();
        let exp = f64::from(l.beat_in_bar - 1) + bpm / 60.0 * since;
        let cur = c.phase(now);
        let bars = ((cur - exp) / crate::clock::BEATS_PER_BAR).round().max(0.0);
        c.follow_phase(exp + bars * crate::clock::BEATS_PER_BAR, now);
        self.state = SyncState::Locked {
            player: l.player,
            bpm,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(t: Instant, n: u64) -> Instant {
        t + Duration::from_millis(n)
    }

    fn beat(player: u8, bpm: f64, pitch: u32, b: u8) -> Beat {
        Beat {
            player,
            bpm,
            pitch,
            beat_in_bar: b,
        }
    }

    fn status(player: u8, master: bool) -> Status {
        Status {
            player,
            master,
            playing: true,
            bpm: 126.0,
            pitch: NORMAL_PITCH,
            beat_count: Some(33),
            beat_in_bar: 2,
        }
    }

    #[test]
    fn beat_roundtrip_and_layout() {
        let b = beat(2, 127.5, NORMAL_PITCH, 3);
        let raw = encode_beat(&b);
        assert_eq!(raw.len(), 0x60);
        assert_eq!(&raw[..10], b"Qspt1WmJOL");
        assert_eq!(raw[0x0a], 0x28);
        assert_eq!(raw[0x21], 2);
        assert_eq!(u16::from_be_bytes([raw[0x5a], raw[0x5b]]), 12750);
        assert_eq!(raw[0x5c], 3);
        assert_eq!(decode(&raw), Some(Packet::Beat(b)));
        let p = beat(1, 90.0, NORMAL_PITCH + 12345, 4);
        assert_eq!(decode(&encode_beat(&p)), Some(Packet::Beat(p)));
    }

    #[test]
    fn status_roundtrip_and_flags() {
        let s = status(3, true);
        let raw = encode_status(&s);
        assert_eq!(raw.len(), 0xd4);
        assert_eq!(raw[0x0a], 0x0a);
        assert_eq!(raw[0x89] & 0x20, 0x20);
        assert_eq!(decode(&raw), Some(Packet::Status(s)));
        let mut n = status(3, false);
        n.playing = false;
        n.beat_count = None;
        assert_eq!(decode(&encode_status(&n)), Some(Packet::Status(n)));
    }

    #[test]
    fn garbage_is_rejected() {
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"hello world, not a packet at all"), None);
        let raw = encode_beat(&beat(1, 120.0, NORMAL_PITCH, 1));
        assert_eq!(decode(&raw[..0x40]), None); // truncated
        let mut bad = raw.clone();
        bad[3] ^= 0xff; // magic broken
        assert_eq!(decode(&bad), None);
        let mut unk = raw;
        unk[0x0a] = 0x7f; // unknown kind
        assert_eq!(decode(&unk), None);
        let mut b0 = encode_beat(&beat(1, 120.0, NORMAL_PITCH, 1));
        b0[0x5c] = 9; // beat in bar out of 1..=4
        assert_eq!(decode(&b0), None);
    }

    #[test]
    fn pitch_scales_tempo() {
        let up = NORMAL_PITCH + NORMAL_PITCH / 16;
        assert!((effective_bpm(100.0, up) - 106.25).abs() < 1e-9);
        assert!((effective_bpm(100.0, NORMAL_PITCH) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn master_flag_picks_the_lead_else_lowest() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Packet::Beat(beat(1, 120.0, NORMAL_PITCH, 1)), t0);
        t.feed(Packet::Beat(beat(2, 128.0, NORMAL_PITCH, 1)), t0);
        assert_eq!(t.lead(0, t0).unwrap().player, 1);
        t.feed(Packet::Status(status(2, true)), t0);
        assert_eq!(t.lead(0, t0).unwrap().player, 2);
        assert_eq!(t.lead(1, t0).unwrap().player, 1); // explicit follow
        assert_eq!(t.lead(4, t0), None);
        assert_eq!(t.players(t0), vec![1, 2]);
        assert_eq!(t.beat_count(2), Some(33));
    }

    #[test]
    fn silent_players_expire() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.feed(Packet::Beat(beat(1, 120.0, NORMAL_PITCH, 1)), t0);
        assert!(t.lead(0, ms(t0, 1900)).is_some());
        assert!(t.lead(0, ms(t0, 2100)).is_none());
        assert!(t.players(ms(t0, 2100)).is_empty());
    }

    /// Synthetic player: beat n (0-based) lands at start + n * 60/bpm.
    fn run(bpm_track: f64, pitch: u32, start_beat: u8, secs: f64) -> (f64, f64) {
        let t0 = Instant::now();
        let eff = effective_bpm(bpm_track, pitch);
        let period = 60.0 / eff;
        let mut clock = Clock::new(100.0, t0);
        let mut t = Tracker::default();
        let mut f = Follower::default();
        let mut next = 0u32;
        let mut worst: f64 = 0.0;
        let mut now_s = 0.0;
        while now_s < secs {
            let now = t0 + Duration::from_secs_f64(now_s);
            while f64::from(next) * period <= now_s {
                let at = t0 + Duration::from_secs_f64(f64::from(next) * period);
                let b = ((u32::from(start_beat) - 1 + next) % 4) as u8 + 1;
                t.feed(Packet::Beat(beat(1, bpm_track, pitch, b)), at);
                next += 1;
            }
            f.update(&t, true, 0, &mut clock, now);
            if now_s > 3.0 {
                // truth: beat index since start, offset by start_beat
                let truth = now_s / period + f64::from(start_beat - 1);
                let mine = clock.phase(now);
                let mut e = (mine - truth) % 4.0;
                if e > 2.0 {
                    e -= 4.0;
                }
                if e < -2.0 {
                    e += 4.0;
                }
                worst = worst.max((e * period * 1000.0).abs());
            }
            now_s += 0.016;
        }
        (worst, clock.bpm())
    }

    #[test]
    fn clock_follows_synthetic_stream_within_5ms() {
        let (err, bpm) = run(128.0, NORMAL_PITCH, 2, 12.0);
        assert!(err < 5.0, "phase error {err} ms");
        assert!((bpm - 128.0).abs() < 0.01);
    }

    #[test]
    fn clock_follows_pitch_and_midbar_start() {
        let up = NORMAL_PITCH + NORMAL_PITCH / 16;
        let (err, bpm) = run(120.0, up, 3, 12.0);
        assert!(err < 5.0, "phase error {err} ms");
        assert!((bpm - 127.5).abs() < 0.01, "{bpm}");
    }

    #[test]
    fn disabled_or_lost_player_leaves_the_clock_alone() {
        let t0 = Instant::now();
        let mut clock = Clock::new(100.0, t0);
        let mut t = Tracker::default();
        let mut f = Follower::default();
        t.feed(Packet::Beat(beat(1, 140.0, NORMAL_PITCH, 1)), t0);
        f.update(&t, false, 0, &mut clock, t0);
        assert_eq!(clock.bpm(), 100.0);
        assert_eq!(f.state, SyncState::Off);
        f.update(&t, true, 0, &mut clock, t0);
        assert_eq!(clock.bpm(), 140.0);
        assert!(matches!(f.state, SyncState::Locked { player: 1, .. }));
        // player vanishes: tempo stays, state says waiting
        f.update(&t, true, 0, &mut clock, ms(t0, 3000));
        assert_eq!(clock.bpm(), 140.0);
        assert_eq!(f.state, SyncState::Waiting);
    }
}
