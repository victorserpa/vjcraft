//! DMX over Art-Net, pure: packet encode/decode (ArtDmx, ArtPoll, ArtPollReply), pixel to
//! channel mapping for fixtures, universe splitting, and DMX as shortcut source. No sockets
//! here (the app owns them), so all of it tests without a network.
//!
//! Layout follows the public Art-Net 4 spec: "Art-Net\0", opcode little endian, protocol
//! version and ArtDmx length big endian, 15-bit port-address = Net (7 bits) + SubUni (8).

use crate::Command;
use crate::shortcuts::{self, Ctx};
use vjc_model::{
    ChannelOrder, ControlSource, Fixture, PixelFormat, Project, Shortcut, ShortcutMode,
};

pub const PORT: u16 = 6454;
pub const HEADER: &[u8; 8] = b"Art-Net\0";
pub const OP_POLL: u16 = 0x2000;
pub const OP_POLL_REPLY: u16 = 0x2100;
pub const OP_DMX: u16 = 0x5000;
pub const PROTOCOL_VERSION: u16 = 14;
pub const MAX_CHANNELS: usize = 512;
/// Highest port-address (15 bits).
pub const MAX_UNIVERSE: u16 = 0x7FFF;
/// Fixtures larger than this are refused (about 190 universes of RGB).
pub const MAX_PIXELS: u32 = 32768;
const POLL_REPLY_LEN: usize = 239;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtDmx {
    pub sequence: u8,
    pub physical: u8,
    /// 15-bit port-address.
    pub universe: u16,
    /// 1..=512 channel values (odd lengths are padded with a zero when encoding).
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtPoll {
    pub flags: u8,
    pub priority: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollReply {
    pub ip: [u8; 4],
    pub port: u16,
    pub net: u8,
    pub sub: u8,
    pub short_name: String,
    pub long_name: String,
    pub num_ports: u16,
    pub mac: [u8; 6],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packet {
    Dmx(ArtDmx),
    Poll(ArtPoll),
    PollReply(PollReply),
}

fn head(op: u16) -> Vec<u8> {
    let mut v = HEADER.to_vec();
    v.extend_from_slice(&op.to_le_bytes());
    v
}

/// ArtDmx packet. None when the universe does not fit 15 bits or there is no data; more than
/// 512 values are cut.
pub fn encode_dmx(d: &ArtDmx) -> Option<Vec<u8>> {
    if d.universe > MAX_UNIVERSE || d.data.is_empty() {
        return None;
    }
    let mut data = d.data[..d.data.len().min(MAX_CHANNELS)].to_vec();
    if data.len() % 2 == 1 {
        data.push(0);
    }
    let mut v = head(OP_DMX);
    v.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    v.push(d.sequence);
    v.push(d.physical);
    v.push((d.universe & 0xFF) as u8);
    v.push((d.universe >> 8) as u8);
    v.extend_from_slice(&(data.len() as u16).to_be_bytes());
    v.extend_from_slice(&data);
    Some(v)
}

pub fn encode_poll(p: &ArtPoll) -> Vec<u8> {
    let mut v = head(OP_POLL);
    v.extend_from_slice(&PROTOCOL_VERSION.to_be_bytes());
    v.push(p.flags);
    v.push(p.priority);
    v
}

fn put_str(buf: &mut [u8], s: &str) {
    let n = s.len().min(buf.len() - 1); // keep the NUL terminator
    buf[..n].copy_from_slice(&s.as_bytes()[..n]);
}

pub fn encode_poll_reply(r: &PollReply) -> Vec<u8> {
    let mut v = head(OP_POLL_REPLY);
    v.extend_from_slice(&r.ip);
    v.extend_from_slice(&r.port.to_le_bytes());
    v.extend_from_slice(&[0, 0]); // firmware version
    v.push(r.net);
    v.push(r.sub);
    v.extend_from_slice(&[0xFF, 0xFF]); // OEM: unknown
    v.push(0); // UBEA version
    v.push(0xE0); // status1: indicators normal, port-address by network
    v.extend_from_slice(&[0, 0]); // ESTA
    let mut short = [0u8; 18];
    put_str(&mut short, &r.short_name);
    v.extend_from_slice(&short);
    let mut long = [0u8; 64];
    put_str(&mut long, &r.long_name);
    v.extend_from_slice(&long);
    let mut report = [0u8; 64];
    put_str(&mut report, "#0001 [0000] VJCraft ready");
    v.extend_from_slice(&report);
    v.extend_from_slice(&r.num_ports.to_be_bytes());
    v.extend_from_slice(&[0x40, 0, 0, 0]); // port types: DMX512 input on port 1
    v.resize(v.len() + 4 * 4, 0); // good input / good output / swin / swout
    v.resize(v.len() + 6, 0); // video, macro, remote, spare
    v.push(0); // style: StNode
    v.extend_from_slice(&r.mac);
    v.extend_from_slice(&r.ip); // bind ip
    v.push(1); // bind index
    v.push(0x08); // status2: 15-bit port-address
    v.resize(POLL_REPLY_LEN, 0);
    v
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

/// Parses one datagram. None for anything that is not a well-formed packet of a known type:
/// wrong header, short, bad length field, or universe above 15 bits. Never panics.
pub fn decode(b: &[u8]) -> Option<Packet> {
    if b.len() < 10 || &b[..8] != HEADER {
        return None;
    }
    match u16::from_le_bytes([b[8], b[9]]) {
        OP_DMX => {
            if b.len() < 18 {
                return None;
            }
            let version = u16::from_be_bytes([b[10], b[11]]);
            let len = usize::from(u16::from_be_bytes([b[16], b[17]]));
            if version < PROTOCOL_VERSION
                || b[15] > 0x7F
                || len == 0
                || len > MAX_CHANNELS
                || b.len() < 18 + len
            {
                return None;
            }
            Some(Packet::Dmx(ArtDmx {
                sequence: b[12],
                physical: b[13],
                universe: u16::from(b[15]) << 8 | u16::from(b[14]),
                data: b[18..18 + len].to_vec(),
            }))
        }
        OP_POLL => {
            if b.len() < 14 {
                return None;
            }
            Some(Packet::Poll(ArtPoll {
                flags: b[12],
                priority: b[13],
            }))
        }
        OP_POLL_REPLY => {
            if b.len() < POLL_REPLY_LEN {
                return None;
            }
            Some(Packet::PollReply(PollReply {
                ip: b[10..14].try_into().ok()?,
                port: u16::from_le_bytes([b[14], b[15]]),
                net: b[18],
                sub: b[19],
                short_name: text(&b[26..44]),
                long_name: text(&b[44..108]),
                num_ports: u16::from_be_bytes([b[172], b[173]]),
                mac: b[201..207].try_into().ok()?,
            }))
        }
        _ => None,
    }
}

// ---- fixtures and pixel mapping ----

/// Predefined fixtures: 1xN strips, 8x8 matrix, RGBW par.
pub fn presets() -> Vec<Fixture> {
    let f = |name: &str, width, height, format, gamma, serpentine| Fixture {
        name: name.into(),
        width,
        height,
        format,
        order: ChannelOrder::Rgb,
        gamma,
        serpentine,
    };
    vec![
        f("Strip 1x16 RGB", 16, 1, PixelFormat::Rgb, 1.0, false),
        f("Strip 1x60 RGB", 60, 1, PixelFormat::Rgb, 2.2, false),
        f("Matrix 8x8 RGB", 8, 8, PixelFormat::Rgb, 2.2, true),
        f("Par RGBW", 1, 1, PixelFormat::Rgbw, 1.0, false),
    ]
}

/// Whether the fixture can be mapped (non-empty, not absurdly large).
pub fn valid(f: &Fixture) -> bool {
    f.width >= 1
        && f.height >= 1
        && u64::from(f.width) * u64::from(f.height) <= u64::from(MAX_PIXELS)
}

/// Channels used by one pixel.
pub fn channels_per_pixel(f: PixelFormat) -> usize {
    match f {
        PixelFormat::Rgb => 3,
        PixelFormat::Rgbw => 4,
        PixelFormat::W => 1,
    }
}

/// Position of (r, g, b) inside the 3 colour channels.
fn order_idx(o: ChannelOrder) -> [usize; 3] {
    match o {
        ChannelOrder::Rgb => [0, 1, 2],
        ChannelOrder::Rbg => [0, 2, 1],
        ChannelOrder::Grb => [1, 0, 2],
        ChannelOrder::Gbr => [2, 0, 1],
        ChannelOrder::Brg => [1, 2, 0],
        ChannelOrder::Bgr => [2, 1, 0],
    }
}

fn gamma_lut(g: f32) -> [u8; 256] {
    let g = if g.is_finite() {
        g.clamp(0.1, 5.0)
    } else {
        1.0
    };
    std::array::from_fn(|i| ((i as f32 / 255.0).powf(g) * 255.0).round() as u8)
}

/// Samples `rgba` (w x h, 4 bytes per pixel) on the fixture grid by averaging each cell's
/// source pixels, applies gamma, and lays the channels out in fixture order. Empty when the
/// fixture is invalid or the buffer is shorter than w*h*4.
pub fn map_pixels(f: &Fixture, rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let need = u64::from(w) * u64::from(h) * 4;
    if !valid(f) || w == 0 || h == 0 || (rgba.len() as u64) < need {
        return vec![];
    }
    let cpp = channels_per_pixel(f.format);
    let lut = gamma_lut(f.gamma);
    let ord = order_idx(f.order);
    let cell = |g: u32, n: u32, src: u32| {
        let a = (u64::from(g) * u64::from(src) / u64::from(n)) as u32;
        let b = ((u64::from(g) + 1) * u64::from(src) / u64::from(n)) as u32;
        (a, b.max(a + 1).min(src))
    };
    let mut out = Vec::with_capacity((f.width * f.height) as usize * cpp);
    for gy in 0..f.height {
        let (y0, y1) = cell(gy, f.height, h);
        for col in 0..f.width {
            let gx = if f.serpentine && gy % 2 == 1 {
                f.width - 1 - col
            } else {
                col
            };
            let (x0, x1) = cell(gx, f.width, w);
            let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
            for y in y0..y1 {
                let row = (y as usize * w as usize + x0 as usize) * 4;
                for px in rgba[row..row + (x1 - x0) as usize * 4].as_chunks::<4>().0 {
                    sr += u64::from(px[0]);
                    sg += u64::from(px[1]);
                    sb += u64::from(px[2]);
                    n += 1;
                }
            }
            let avg = |s: u64| ((s + n / 2) / n.max(1)) as u8;
            let (r, g, b) = (avg(sr), avg(sg), avg(sb));
            match f.format {
                PixelFormat::W => {
                    let y = (2126 * u32::from(r) + 7152 * u32::from(g) + 722 * u32::from(b) + 5000)
                        / 10000;
                    out.push(lut[y.min(255) as usize]);
                }
                PixelFormat::Rgb | PixelFormat::Rgbw => {
                    // RGBW: the common white moves to W so the colour LEDs carry the rest.
                    let white = if f.format == PixelFormat::Rgbw {
                        r.min(g).min(b)
                    } else {
                        0
                    };
                    let c = [r - white, g - white, b - white];
                    let mut px = [0u8; 3];
                    for (i, v) in c.iter().enumerate() {
                        px[ord[i]] = lut[usize::from(*v)];
                    }
                    out.extend_from_slice(&px);
                    if f.format == PixelFormat::Rgbw {
                        out.push(lut[usize::from(white)]);
                    }
                }
            }
        }
    }
    out
}

/// Splits channel data over consecutive universes starting at `start`, `chunk` values each
/// (see `universe_chunk`, so pixels never straddle two universes). Stops at the 15-bit limit.
pub fn split_universes(start: u16, data: &[u8], chunk: usize) -> Vec<(u16, &[u8])> {
    let chunk = chunk.clamp(1, MAX_CHANNELS);
    data.chunks(chunk)
        .enumerate()
        .map_while(|(i, c)| {
            let u = u32::from(start) + i as u32;
            (u <= u32::from(MAX_UNIVERSE)).then_some((u as u16, c))
        })
        .collect()
}

/// Channels per universe that hold whole pixels (510 for RGB, 512 for RGBW and W).
pub fn universe_chunk(f: PixelFormat) -> usize {
    MAX_CHANNELS / channels_per_pixel(f) * channels_per_pixel(f)
}

/// ArtDmx packets for one frame: the fixture sampled from `rgba`, split over universes
/// from `start`. `seq` is the Art-Net sequence byte (1..=255; 0 disables ordering).
pub fn frame_packets(
    f: &Fixture,
    start: u16,
    rgba: &[u8],
    (w, h): (u32, u32),
    seq: u8,
) -> Vec<Vec<u8>> {
    let data = map_pixels(f, rgba, w, h);
    split_universes(start, &data, universe_chunk(f.format))
        .into_iter()
        .filter_map(|(universe, d)| {
            encode_dmx(&ArtDmx {
                sequence: seq,
                physical: 0,
                universe,
                data: d.to_vec(),
            })
        })
        .collect()
}

/// Next sequence number, skipping 0.
pub fn next_seq(seq: u8) -> u8 {
    if seq == 255 { 1 } else { seq + 1 }
}

// ---- DMX as shortcut source ----

/// One channel value that arrived (channel is 1-based, 1..=512).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub universe: u16,
    pub channel: u16,
    pub value: u8,
}

/// The control an event comes from, for learn.
pub fn source_of(ev: &Event) -> ControlSource {
    ControlSource::Dmx {
        universe: ev.universe,
        channel: ev.channel,
    }
}

fn is_dmx(src: &ControlSource) -> bool {
    matches!(src, ControlSource::Dmx { .. })
}

/// Command for a channel value that hits `s`, if any.
/// - Absolute: 0..255 maps linearly onto `range` (reversed when `invert`).
/// - Relative: 128 = still, above up, below down; each step moves (hi - lo) / 255.
/// - Trigger/Toggle: acts like a press when the value is >= 128.
pub fn command_for(p: &Project, ctx: &Ctx, s: &Shortcut, ev: &Event) -> Option<Command> {
    let ControlSource::Dmx { universe, channel } = s.source else {
        return None;
    };
    if universe != ev.universe || channel != ev.channel {
        return None;
    }
    let (lo, hi) = s.range;
    match s.mode {
        ShortcutMode::Absolute => {
            let t = f64::from(ev.value) / 255.0;
            let t = if s.invert { 1.0 - t } else { t };
            shortcuts::resolve_with(p, ctx, &s.target, lo + (hi - lo) * t)
        }
        ShortcutMode::Relative => {
            let delta = i32::from(ev.value) - 128;
            if delta == 0 {
                return None;
            }
            let sign = if s.invert { -1.0 } else { 1.0 };
            let cur = shortcuts::current(p, ctx, &s.target)?;
            let v = cur + sign * f64::from(delta) * (hi - lo) / 255.0;
            shortcuts::resolve_with(p, ctx, &s.target, v)
        }
        ShortcutMode::Trigger | ShortcutMode::Toggle => {
            if ev.value >= 128 {
                shortcuts::activate(p, ctx, s)
            } else {
                None
            }
        }
    }
}

/// Commands that remove every DMX shortcut of `target`.
pub fn unlearn(p: &Project, target: &str) -> Vec<Command> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target && is_dmx(&s.source))
        .map(|s| Command::RemoveShortcut { id: s.id })
        .collect()
}

/// Commands that map the DMX source to `target` (old DMX shortcut of the target and any
/// other target on the same channel go away first). Continuous targets and bpm are
/// Absolute faders; bypass is a toggle; the rest triggers.
pub fn learn(p: &Project, target: &str, source: ControlSource) -> Vec<Command> {
    let same = |s: &Shortcut| s.target == target && s.source == source;
    let mut cmds: Vec<Command> = p
        .shortcuts
        .iter()
        .filter(|s| !same(s) && ((s.target == target && is_dmx(&s.source)) || s.source == source))
        .map(|s| Command::RemoveShortcut { id: s.id })
        .collect();
    // A fader keeps streaming values: once mapped, further values change nothing.
    if p.shortcuts.iter().any(same) {
        return cmds;
    }
    let (mode, range) = if shortcuts::is_continuous(target) {
        (ShortcutMode::Absolute, (0.0, 1.0))
    } else if target.ends_with("/bpm") {
        (
            ShortcutMode::Absolute,
            (crate::clock::MIN_BPM, crate::clock::MAX_BPM),
        )
    } else if target.ends_with("/bypass") {
        (ShortcutMode::Toggle, (0.0, 1.0))
    } else {
        (ShortcutMode::Trigger, (0.0, 1.0))
    };
    cmds.push(Command::AddShortcut {
        shortcut: Shortcut {
            id: uuid::Uuid::new_v4(),
            source,
            target: target.into(),
            mode,
            range,
            invert: false,
        },
        index: p.shortcuts.len(),
    });
    cmds
}

/// "DMX 1.5" (universe.channel) label of the control mapped to `target`, for the overlay.
pub fn label_for(p: &Project, target: &str) -> Option<String> {
    p.shortcuts
        .iter()
        .filter(|s| s.target == target)
        .find_map(|s| match s.source {
            ControlSource::Dmx { universe, channel } => Some(format!("DMX {universe}.{channel}")),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dmx(universe: u16, n: usize) -> ArtDmx {
        ArtDmx {
            sequence: 7,
            physical: 1,
            universe,
            data: (0..n).map(|i| i as u8).collect(),
        }
    }

    #[test]
    fn dmx_round_trip_and_wire_layout() {
        let d = dmx(0x1234, 10);
        let b = encode_dmx(&d).unwrap();
        assert_eq!(&b[..8], b"Art-Net\0");
        assert_eq!(&b[8..10], &[0x00, 0x50]); // opcode 0x5000 little endian
        assert_eq!(&b[10..12], &[0, 14]);
        assert_eq!((b[14], b[15]), (0x34, 0x12)); // SubUni, Net
        assert_eq!(&b[16..18], &[0, 10]);
        assert_eq!(decode(&b), Some(Packet::Dmx(d)));
    }

    #[test]
    fn odd_length_is_padded_and_long_data_cut() {
        let b = encode_dmx(&dmx(0, 3)).unwrap();
        assert_eq!(&b[16..18], &[0, 4]);
        let Some(Packet::Dmx(d)) = decode(&b) else {
            panic!()
        };
        assert_eq!(d.data, vec![0, 1, 2, 0]);
        let b = encode_dmx(&dmx(0, 600)).unwrap();
        assert_eq!(b.len(), 18 + 512);
    }

    #[test]
    fn universe_is_limited_to_15_bits() {
        assert!(encode_dmx(&dmx(0x7FFF, 2)).is_some());
        assert!(encode_dmx(&dmx(0x8000, 2)).is_none());
        let mut b = encode_dmx(&dmx(1, 2)).unwrap();
        b[15] = 0x80;
        assert_eq!(decode(&b), None);
    }

    #[test]
    fn malformed_packets_are_none_never_panic() {
        let good = encode_dmx(&dmx(1, 8)).unwrap();
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(b"Art-Net"), None);
        let mut bad = good.clone();
        bad[0] = b'X';
        assert_eq!(decode(&bad), None);
        bad = good.clone();
        bad[7] = 1; // header must end in NUL
        assert_eq!(decode(&bad), None);
        assert_eq!(decode(&good[..good.len() - 1]), None); // length field > payload
        bad = good.clone();
        bad[16] = 0xFF; // length 0xFF08
        assert_eq!(decode(&bad), None);
        bad = good.clone();
        bad[16] = 0;
        bad[17] = 0; // zero length
        assert_eq!(decode(&bad), None);
        bad = good.clone();
        bad[11] = 3; // old protocol version
        assert_eq!(decode(&bad), None);
        bad = good.clone();
        bad[8] = 0x99; // unknown opcode
        assert_eq!(decode(&bad), None);
        for n in 0..good.len() {
            let _ = decode(&good[..n]);
        }
        let reply = encode_poll_reply(&PollReply {
            ip: [1, 2, 3, 4],
            port: PORT,
            net: 0,
            sub: 0,
            short_name: "a".into(),
            long_name: "b".into(),
            num_ports: 1,
            mac: [0; 6],
        });
        for n in 0..reply.len() {
            assert!(decode(&reply[..n]).is_none());
        }
    }

    #[test]
    fn poll_and_reply_round_trip() {
        let p = ArtPoll {
            flags: 2,
            priority: 0x10,
        };
        let b = encode_poll(&p);
        assert_eq!(&b[8..10], &[0x00, 0x20]);
        assert_eq!(decode(&b), Some(Packet::Poll(p)));
        assert_eq!(decode(&b[..13]), None);
        let r = PollReply {
            ip: [192, 168, 1, 9],
            port: PORT,
            net: 1,
            sub: 2,
            short_name: "VJCraft".into(),
            long_name: "x".repeat(200),
            num_ports: 1,
            mac: [1, 2, 3, 4, 5, 6],
        };
        let b = encode_poll_reply(&r);
        assert_eq!(b.len(), 239);
        assert_eq!(&b[8..10], &[0x00, 0x21]);
        assert_eq!(&b[14..16], &[0x36, 0x19]); // 6454 little endian
        let Some(Packet::PollReply(back)) = decode(&b) else {
            panic!()
        };
        assert_eq!(back.long_name.len(), 63); // cut to leave the NUL
        assert_eq!(
            (
                back.ip,
                back.port,
                back.net,
                back.sub,
                back.num_ports,
                back.mac
            ),
            (r.ip, r.port, r.net, r.sub, r.num_ports, r.mac)
        );
        assert_eq!(back.short_name, "VJCraft");
    }

    fn fx(format: PixelFormat, w: u32, h: u32) -> Fixture {
        Fixture {
            name: "t".into(),
            width: w,
            height: h,
            format,
            order: ChannelOrder::Rgb,
            gamma: 1.0,
            serpentine: false,
        }
    }

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
        px.iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect()
    }

    #[test]
    fn maps_pixels_row_major_one_to_one() {
        // 2x2: red green / blue white
        let img = [
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let out = map_pixels(&fx(PixelFormat::Rgb, 2, 2), &img, 2, 2);
        assert_eq!(out, [255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    }

    #[test]
    fn channel_order_is_applied() {
        let img = solid(1, 1, [10, 20, 30, 255]);
        let order = |o| {
            let mut f = fx(PixelFormat::Rgb, 1, 1);
            f.order = o;
            map_pixels(&f, &img, 1, 1)
        };
        assert_eq!(order(ChannelOrder::Rgb), [10, 20, 30]);
        assert_eq!(order(ChannelOrder::Grb), [20, 10, 30]);
        assert_eq!(order(ChannelOrder::Bgr), [30, 20, 10]);
        assert_eq!(order(ChannelOrder::Rbg), [10, 30, 20]);
        assert_eq!(order(ChannelOrder::Gbr), [20, 30, 10]);
        assert_eq!(order(ChannelOrder::Brg), [30, 10, 20]);
    }

    #[test]
    fn gamma_curve_is_applied() {
        let img = solid(1, 1, [128, 255, 0, 255]);
        let mut f = fx(PixelFormat::Rgb, 1, 1);
        f.gamma = 2.0;
        // (128/255)^2 * 255 = 64.25
        assert_eq!(map_pixels(&f, &img, 1, 1), [64, 255, 0]);
        f.gamma = f32::NAN; // garbage falls back to linear
        assert_eq!(map_pixels(&f, &img, 1, 1), [128, 255, 0]);
    }

    #[test]
    fn rgbw_moves_common_white_to_w() {
        let img = solid(1, 1, [200, 100, 50, 255]);
        let out = map_pixels(&fx(PixelFormat::Rgbw, 1, 1), &img, 1, 1);
        assert_eq!(out, [150, 50, 0, 50]);
        let white = solid(1, 1, [255, 255, 255, 255]);
        assert_eq!(
            map_pixels(&fx(PixelFormat::Rgbw, 1, 1), &white, 1, 1),
            [0, 0, 0, 255]
        );
    }

    #[test]
    fn w_format_uses_luminance() {
        let img = solid(1, 1, [255, 255, 255, 255]);
        assert_eq!(map_pixels(&fx(PixelFormat::W, 1, 1), &img, 1, 1), [255]);
        let red = solid(1, 1, [255, 0, 0, 255]);
        assert_eq!(map_pixels(&fx(PixelFormat::W, 1, 1), &red, 1, 1), [54]);
    }

    #[test]
    fn downsample_averages_cells() {
        // 4x1 source: 0 0 | 200 100 on the red channel -> 2 cells
        let mut img = vec![0u8; 16];
        img[8] = 200;
        img[12] = 100;
        let out = map_pixels(&fx(PixelFormat::Rgb, 2, 1), &img, 4, 1);
        assert_eq!([out[0], out[3]], [0, 150]);
        // upscale: 1 pixel feeds every cell
        let one = solid(1, 1, [9, 9, 9, 255]);
        assert_eq!(
            map_pixels(&fx(PixelFormat::Rgb, 3, 3), &one, 1, 1).len(),
            27
        );
    }

    #[test]
    fn serpentine_reverses_odd_rows() {
        let img = [
            1, 0, 0, 255, 2, 0, 0, 255, //
            3, 0, 0, 255, 4, 0, 0, 255,
        ];
        let mut f = fx(PixelFormat::W, 2, 2);
        f.gamma = 1.0;
        f.serpentine = true;
        let out = map_pixels(&f, &img, 2, 2);
        // luminance of (n,0,0) is n*0.2126 -> 0; use RGB instead
        assert_eq!(out.len(), 4);
        f.format = PixelFormat::Rgb;
        let out = map_pixels(&f, &img, 2, 2);
        assert_eq!([out[0], out[3], out[6], out[9]], [1, 2, 4, 3]);
    }

    #[test]
    fn invalid_input_gives_no_channels() {
        let img = solid(2, 2, [1, 1, 1, 255]);
        assert!(map_pixels(&fx(PixelFormat::Rgb, 0, 1), &img, 2, 2).is_empty());
        assert!(map_pixels(&fx(PixelFormat::Rgb, 1, 1), &img[..8], 2, 2).is_empty());
        assert!(map_pixels(&fx(PixelFormat::Rgb, 1, 1), &img, 0, 2).is_empty());
        assert!(map_pixels(&fx(PixelFormat::Rgb, 40000, 40000), &img, 2, 2).is_empty());
    }

    #[test]
    fn universes_split_on_pixel_boundaries() {
        let f = fx(PixelFormat::Rgb, 200, 1); // 600 channels
        let img = solid(200, 1, [5, 5, 5, 255]);
        let data = map_pixels(&f, &img, 200, 1);
        let parts = split_universes(3, &data, universe_chunk(f.format));
        assert_eq!(universe_chunk(PixelFormat::Rgb), 510);
        assert_eq!(universe_chunk(PixelFormat::Rgbw), 512);
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].0, parts[0].1.len()), (3, 510));
        assert_eq!((parts[1].0, parts[1].1.len()), (4, 90));
        // limit of 15 bits
        let p = split_universes(0x7FFF, &data, 510);
        assert_eq!(p.len(), 1);
        assert!(split_universes(0, &[], 510).is_empty());
    }

    #[test]
    fn frame_packets_are_decodable_and_consecutive() {
        let f = fx(PixelFormat::Rgbw, 200, 1); // 800 channels
        let img = solid(200, 1, [10, 20, 30, 255]);
        let pk = frame_packets(&f, 10, &img, (200, 1), 5);
        assert_eq!(pk.len(), 2);
        let ds: Vec<ArtDmx> = pk
            .iter()
            .map(|b| match decode(b) {
                Some(Packet::Dmx(d)) => d,
                _ => panic!(),
            })
            .collect();
        assert_eq!((ds[0].universe, ds[1].universe), (10, 11));
        assert_eq!((ds[0].data.len(), ds[1].data.len()), (512, 288));
        assert_eq!(&ds[0].data[..4], &[0, 10, 20, 10]);
        assert_eq!(ds[0].sequence, 5);
        assert_eq!(next_seq(255), 1);
        assert_eq!(next_seq(1), 2);
    }

    #[test]
    fn presets_are_valid() {
        let p = presets();
        assert!(p.len() >= 3 && p.iter().all(valid));
        assert!(p.iter().any(|f| f.format == PixelFormat::Rgbw));
        assert!(p.iter().any(|f| f.width == 8 && f.height == 8));
        assert!(p.iter().any(|f| f.height == 1 && f.width > 1));
    }

    fn fader() -> (Project, Shortcut) {
        let p = Project::new("t");
        let s = Shortcut {
            id: uuid::Uuid::new_v4(),
            source: ControlSource::Dmx {
                universe: 1,
                channel: 5,
            },
            target: "/comp/master/opacity".into(),
            mode: ShortcutMode::Absolute,
            range: (0.0, 1.0),
            invert: false,
        };
        (p, s)
    }

    fn ev(universe: u16, channel: u16, value: u8) -> Event {
        Event {
            universe,
            channel,
            value,
        }
    }

    #[test]
    fn absolute_channel_scales_0_255_to_range() {
        let (p, s) = fader();
        let ctx = Ctx::new(None, false);
        let v = |e| match command_for(&p, &ctx, &s, &e) {
            Some(Command::SetMasterOpacity { value }) => value,
            other => panic!("{other:?}"),
        };
        assert_eq!(v(ev(1, 5, 255)), 1.0);
        assert_eq!(v(ev(1, 5, 0)), 0.0);
        assert!((v(ev(1, 5, 51)) - 0.2).abs() < 1e-9);
        assert!(command_for(&p, &ctx, &s, &ev(1, 6, 9)).is_none());
        assert!(command_for(&p, &ctx, &s, &ev(2, 5, 9)).is_none());
        let inv = Shortcut { invert: true, ..s };
        assert_eq!(
            match command_for(&p, &ctx, &inv, &ev(1, 5, 255)) {
                Some(Command::SetMasterOpacity { value }) => value,
                _ => panic!(),
            },
            0.0
        );
    }

    #[test]
    fn learn_maps_channel_as_absolute_and_replaces_old() {
        let p = Project::new("t");
        let src = source_of(&ev(0, 3, 9));
        let cmds = learn(&p, "/comp/master/opacity", src.clone());
        assert_eq!(cmds.len(), 1);
        let Command::AddShortcut { shortcut, .. } = &cmds[0] else {
            panic!()
        };
        assert_eq!(shortcut.mode, ShortcutMode::Absolute);
        assert_eq!(shortcut.range, (0.0, 1.0));
        let mut p2 = p.clone();
        p2.shortcuts.push(shortcut.clone());
        // same mapping again: nothing to do; another target on the channel: old one goes
        assert!(learn(&p2, "/comp/master/opacity", src.clone()).is_empty());
        let moved = learn(&p2, "/comp/tempo/bpm", src);
        assert!(matches!(moved[0], Command::RemoveShortcut { .. }));
        assert_eq!(moved.len(), 2);
        assert_eq!(unlearn(&p2, "/comp/master/opacity").len(), 1);
        assert_eq!(
            label_for(&p2, "/comp/master/opacity").as_deref(),
            Some("DMX 0.3")
        );
    }
}
