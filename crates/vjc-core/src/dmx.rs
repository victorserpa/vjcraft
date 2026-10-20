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
        ..Fixture::default()
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
    (1..=MAX_CHANNELS as u16).contains(&f.start_channel)
        && f.input.w.is_finite()
        && f.input.h.is_finite()
        && f.input.x.is_finite()
        && f.input.y.is_finite()
        && f.input.rotation.is_finite()
        && f.input.w > 0.0
        && f.input.h > 0.0
        && f.width >= 1
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

/// Fixture colour on an averaged pixel: brightness and white balance, then contrast around
/// mid grey.
fn tint(c: &vjc_model::SliceColor, rgb: [u8; 3]) -> [u8; 3] {
    let gain = [c.r, c.g, c.b];
    std::array::from_fn(|i| {
        let v = f32::from(rgb[i]) / 255.0 * c.brightness * gain[i];
        let v = (v - 0.5) * c.contrast + 0.5;
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

/// Samples `rgba` (w x h, 4 bytes per pixel) on the fixture grid, applies the fixture colour
/// and gamma, and lays the channels out in fixture order. With the default input (the whole
/// screen) each grid cell averages its source pixels; with a moved, scaled or rotated input
/// rectangle each cell averages a 3x3 set of points inside it (black outside the screen).
/// Flips mirror the picture, not the channel order. Empty when the fixture is invalid or the
/// buffer is shorter than w*h*4.
pub fn map_pixels(f: &Fixture, rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let need = u64::from(w) * u64::from(h) * 4;
    if !valid(f) || w == 0 || h == 0 || (rgba.len() as u64) < need {
        return vec![];
    }
    let cpp = channels_per_pixel(f.format);
    let lut = gamma_lut(f.gamma);
    let ord = order_idx(f.order);
    let plain = f.input.is_full();
    let tinted = f.color != vjc_model::SliceColor::default();
    let cell = |g: u32, n: u32, src: u32| {
        let a = (u64::from(g) * u64::from(src) / u64::from(n)) as u32;
        let b = ((u64::from(g) + 1) * u64::from(src) / u64::from(n)) as u32;
        (a, b.max(a + 1).min(src))
    };
    let (sin, cos) = f.input.rotation.to_radians().sin_cos();
    let (fw, fh) = (w as f32, h as f32);
    const SUB: u32 = 3;
    // Average colour of grid cell (cx, cy).
    let sample = |cx: u32, cy: u32| -> [u8; 3] {
        let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
        if plain {
            let (x0, x1) = cell(cx, f.width, w);
            let (y0, y1) = cell(cy, f.height, h);
            for y in y0..y1 {
                let row = (y as usize * w as usize + x0 as usize) * 4;
                for px in rgba[row..row + (x1 - x0) as usize * 4].as_chunks::<4>().0 {
                    sr += u64::from(px[0]);
                    sg += u64::from(px[1]);
                    sb += u64::from(px[2]);
                    n += 1;
                }
            }
        } else {
            for j in 0..SUB {
                for i in 0..SUB {
                    let u = (cx as f32 + (i as f32 + 0.5) / SUB as f32) / f.width as f32 - 0.5;
                    let v = (cy as f32 + (j as f32 + 0.5) / SUB as f32) / f.height as f32 - 0.5;
                    let (lx, ly) = (u * f.input.w * fw, v * f.input.h * fh);
                    let px = f.input.x * fw + lx * cos - ly * sin;
                    let py = f.input.y * fh + lx * sin + ly * cos;
                    n += 1;
                    if px >= 0.0 && py >= 0.0 && px < fw && py < fh {
                        let k = (py as usize * w as usize + px as usize) * 4;
                        sr += u64::from(rgba[k]);
                        sg += u64::from(rgba[k + 1]);
                        sb += u64::from(rgba[k + 2]);
                    }
                }
            }
        }
        let avg = |s: u64| ((s + n / 2) / n.max(1)) as u8;
        [avg(sr), avg(sg), avg(sb)]
    };
    let mut out = Vec::with_capacity((f.width * f.height) as usize * cpp);
    for gy in 0..f.height {
        let cy = if f.flip_v { f.height - 1 - gy } else { gy };
        for col in 0..f.width {
            let gx = if f.serpentine && gy % 2 == 1 {
                f.width - 1 - col
            } else {
                col
            };
            let cx = if f.flip_h { f.width - 1 - gx } else { gx };
            let mut rgb = sample(cx, cy);
            if tinted {
                rgb = tint(&f.color, rgb);
            }
            let [r, g, b] = rgb;
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

/// Splits channel data over consecutive universes. `start_channel` (1..=512) is where the
/// data begins inside universe `start`; `cpp` channels per pixel keep pixels from straddling
/// two universes (when not even one fits in the rest of a universe, the data moves to the
/// next one). Each part is `(universe, first channel 0-based, values)`. Stops at the 15-bit
/// limit.
pub fn split_universes(
    start: u16,
    start_channel: u16,
    data: &[u8],
    cpp: usize,
) -> Vec<(u16, usize, &[u8])> {
    let cpp = cpp.clamp(1, MAX_CHANNELS);
    let mut offset = usize::from(start_channel.clamp(1, MAX_CHANNELS as u16)) - 1;
    let mut universe = u32::from(start);
    if (MAX_CHANNELS - offset) / cpp == 0 {
        offset = 0;
        universe += 1;
    }
    let mut rest = data;
    let mut out = vec![];
    while !rest.is_empty() && universe <= u32::from(MAX_UNIVERSE) {
        let room = (MAX_CHANNELS - offset) / cpp * cpp;
        let (head, tail) = rest.split_at(room.min(rest.len()));
        out.push((universe as u16, offset, head));
        rest = tail;
        offset = 0;
        universe += 1;
    }
    out
}

/// Channels per universe that hold whole pixels (510 for RGB, 512 for RGBW and W).
pub fn universe_chunk(f: PixelFormat) -> usize {
    MAX_CHANNELS / channels_per_pixel(f) * channels_per_pixel(f)
}

/// How many universes `channels` values starting at `start_channel` span (same rule as
/// `split_universes`).
pub fn universes_used(start_channel: u16, channels: usize, cpp: usize) -> usize {
    let cpp = cpp.clamp(1, MAX_CHANNELS);
    if channels == 0 {
        return 0;
    }
    let offset = usize::from(start_channel.clamp(1, MAX_CHANNELS as u16)) - 1;
    let first = (MAX_CHANNELS - offset) / cpp * cpp;
    let per = MAX_CHANNELS / cpp * cpp;
    if first == 0 {
        channels.div_ceil(per)
    } else {
        1 + channels.saturating_sub(first).div_ceil(per)
    }
}

/// ArtDmx packets for one frame: every fixture sampled from `rgba` and placed at its
/// `start_channel`, from universe `start` upward. Fixtures that share a universe go in the
/// same packet; where they overlap the later one wins. `black` sends zeros with the same
/// layout, whatever the fixture colour does. `seq` is the Art-Net sequence byte (1..=255; 0
/// disables ordering).
pub fn frame_packets(
    fixtures: &[Fixture],
    start: u16,
    rgba: &[u8],
    (w, h): (u32, u32),
    seq: u8,
    black: bool,
) -> Vec<Vec<u8>> {
    frame_packets_with(fixtures, start, seq, black, |f| map_pixels(f, rgba, w, h))
}

/// Like `frame_packets`, but `data` supplies each fixture's channel bytes (so a caller that
/// already reduced the picture to the fixture grids can use `map_pixels` on those).
pub fn frame_packets_with(
    fixtures: &[Fixture],
    start: u16,
    seq: u8,
    black: bool,
    mut data_of: impl FnMut(&Fixture) -> Vec<u8>,
) -> Vec<Vec<u8>> {
    let mut unis: std::collections::BTreeMap<u16, Vec<u8>> = Default::default();
    for f in fixtures {
        let mut data = data_of(f);
        if black {
            data.fill(0);
        }
        let cpp = channels_per_pixel(f.format);
        for (u, off, d) in split_universes(start, f.start_channel, &data, cpp) {
            let buf = unis.entry(u).or_default();
            if buf.len() < off + d.len() {
                buf.resize(off + d.len(), 0);
            }
            buf[off..off + d.len()].copy_from_slice(d);
        }
    }
    unis.into_iter()
        .filter_map(|(universe, data)| {
            encode_dmx(&ArtDmx {
                sequence: seq,
                physical: 0,
                universe,
                data,
            })
        })
        .collect()
}

/// Net (7 bits), sub-net (4) and universe (4) of a 15-bit port-address.
pub fn split_port(port: u16) -> (u8, u8, u8) {
    (
        ((port >> 8) & 0x7F) as u8,
        ((port >> 4) & 0x0F) as u8,
        (port & 0x0F) as u8,
    )
}

/// Port-address from net, sub-net and universe (each clamped to its width).
pub fn join_port(net: u8, sub: u8, uni: u8) -> u16 {
    u16::from(net.min(0x7F)) << 8 | u16::from(sub.min(0x0F)) << 4 | u16::from(uni.min(0x0F))
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
            effect: None,
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

/// Fixture input rectangle covering a slice's output area on a `width` x `height` screen
/// (normalized centre and size, no rotation). `None` for an empty or degenerate slice.
pub fn input_for_slice(
    slice: &vjc_model::Slice,
    width: u32,
    height: u32,
) -> Option<vjc_model::FixtureInput> {
    let r = crate::slice_geom::bbox(&slice.output_mesh)?;
    let (sw, sh) = (width as f32, height as f32);
    if sw <= 0.0 || sh <= 0.0 || r.w <= 0.0 || r.h <= 0.0 {
        return None;
    }
    let (cx, cy) = r.center();
    Some(vjc_model::FixtureInput {
        x: cx / sw,
        y: cy / sh,
        w: r.w / sw,
        h: r.h / sh,
        rotation: 0.0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_for_slice_is_the_normalized_output_bbox() {
        let mut s = crate::mapping::new_slice(400, 200);
        s.output_mesh = vec![(100.0, 50.0), (300.0, 50.0), (100.0, 150.0), (300.0, 150.0)];
        s.mesh_cols = 2;
        let i = input_for_slice(&s, 400, 200).unwrap();
        assert_eq!((i.x, i.y, i.w, i.h), (0.5, 0.5, 0.5, 0.5));
        s.output_mesh.clear();
        assert!(input_for_slice(&s, 400, 200).is_none());
        // A point mesh has no area.
        s.output_mesh = vec![(5.0, 5.0)];
        assert!(input_for_slice(&s, 400, 200).is_none());
    }

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
            ..Fixture::default()
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
        let parts = split_universes(3, 1, &data, 3);
        assert_eq!(universe_chunk(PixelFormat::Rgb), 510);
        assert_eq!(universe_chunk(PixelFormat::Rgbw), 512);
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].0, parts[0].2.len()), (3, 510));
        assert_eq!((parts[1].0, parts[1].2.len()), (4, 90));
        // limit of 15 bits
        let p = split_universes(0x7FFF, 1, &data, 3);
        assert_eq!(p.len(), 1);
        assert!(split_universes(0, 1, &[], 3).is_empty());
    }

    #[test]
    fn frame_packets_are_decodable_and_consecutive() {
        let f = fx(PixelFormat::Rgbw, 200, 1); // 800 channels
        let img = solid(200, 1, [10, 20, 30, 255]);
        let pk = frame_packets(&[f], 10, &img, (200, 1), 5, false);
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

    fn decode_all(pk: &[Vec<u8>]) -> Vec<ArtDmx> {
        pk.iter()
            .map(|b| match decode(b) {
                Some(Packet::Dmx(d)) => d,
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn start_channel_shifts_the_channels() {
        let img = solid(1, 1, [9, 8, 7, 255]);
        let mut f = fx(PixelFormat::Rgb, 1, 1);
        let at = |f: &Fixture| {
            decode_all(&frame_packets(
                std::slice::from_ref(f),
                2,
                &img,
                (1, 1),
                1,
                false,
            ))
        };
        assert_eq!(at(&f)[0].data, [9, 8, 7, 0]); // padded to even length
        f.start_channel = 11;
        let d = &at(&f)[0];
        assert_eq!(d.universe, 2);
        assert_eq!(&d.data[..10], &[0; 10]);
        assert_eq!(&d.data[10..13], &[9, 8, 7]);
        // No room for a whole pixel in the rest of the universe: next universe, channel 1.
        let p = split_universes(2, 512, &[1, 2, 3], 3);
        assert_eq!((p[0].0, p[0].1), (3, 0));
        // Pixels never straddle: 2 RGB pixels from channel 509 -> one fits (509..511 is
        // 3 channels), the second moves on.
        let p = split_universes(0, 509, &[0; 6], 3);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].1, p[0].2.len(), p[1].0, p[1].1), (508, 3, 1, 0));
    }

    #[test]
    fn two_fixtures_in_one_universe_do_not_overlap() {
        let img = solid(1, 1, [1, 2, 3, 255]);
        let a = fx(PixelFormat::Rgb, 2, 1); // channels 1..=6
        let mut b = fx(PixelFormat::Rgb, 1, 1);
        b.start_channel = 7;
        b.color.brightness = 0.0; // distinguishable from `a`: zeros
        let pk = frame_packets(&[a, b.clone()], 0, &img, (1, 1), 1, false);
        assert_eq!(pk.len(), 1);
        let d = &decode_all(&pk)[0];
        assert_eq!(d.data, [1, 2, 3, 1, 2, 3, 0, 0, 0, 0]);
        // Negative control: starting at the same channel, b overwrites a.
        b.start_channel = 1;
        let d = &decode_all(&frame_packets(
            &[fx(PixelFormat::Rgb, 2, 1), b],
            0,
            &img,
            (1, 1),
            1,
            false,
        ))[0];
        assert_eq!(&d.data[..6], &[0, 0, 0, 1, 2, 3]);
    }

    #[test]
    fn flip_reverses_the_order_and_only_then() {
        let img = [10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255];
        let mut f = fx(PixelFormat::Rgb, 3, 1);
        assert_eq!(map_pixels(&f, &img, 3, 1), [10, 0, 0, 20, 0, 0, 30, 0, 0]);
        f.flip_h = true;
        assert_eq!(map_pixels(&f, &img, 3, 1), [30, 0, 0, 20, 0, 0, 10, 0, 0]);
        f.flip_h = false;
        f.flip_v = true; // one row: unchanged
        assert_eq!(map_pixels(&f, &img, 3, 1), [10, 0, 0, 20, 0, 0, 30, 0, 0]);
        let tall = [1, 0, 0, 255, 2, 0, 0, 255];
        let mut g = fx(PixelFormat::Rgb, 1, 2);
        g.flip_v = true;
        assert_eq!(map_pixels(&g, &tall, 1, 2), [2, 0, 0, 1, 0, 0]);
    }

    #[test]
    fn input_rect_moves_scales_and_rotates() {
        // 4x1 image: 10 20 30 40 in red.
        let img = [10, 0, 0, 255, 20, 0, 0, 255, 30, 0, 0, 255, 40, 0, 0, 255];
        let mut f = fx(PixelFormat::Rgb, 2, 1);
        f.input.w = 0.5; // right half, centred at 0.75
        f.input.x = 0.75;
        let r: Vec<u8> = map_pixels(&f, &img, 4, 1).chunks(3).map(|c| c[0]).collect();
        assert_eq!(r, [30, 40]);
        // 180 degrees about the centre of the screen reverses the picture.
        let mut g = fx(PixelFormat::Rgb, 4, 1);
        g.input.rotation = 180.0;
        let r: Vec<u8> = map_pixels(&g, &img, 4, 1).chunks(3).map(|c| c[0]).collect();
        assert_eq!(r, [40, 30, 20, 10]);
        // Outside the screen is black.
        let mut o = fx(PixelFormat::Rgb, 1, 1);
        o.input.x = 3.0;
        assert_eq!(map_pixels(&o, &img, 4, 1), [0, 0, 0]);
    }

    #[test]
    fn fixture_colour_applies_and_black_ignores_it() {
        let img = solid(1, 1, [100, 100, 100, 255]);
        let mut f = fx(PixelFormat::Rgb, 1, 1);
        f.color.brightness = 0.5;
        f.color.b = 0.0;
        assert_eq!(map_pixels(&f, &img, 1, 1), [50, 50, 0]);
        // Contrast below 1 lifts black to grey; the black-out must still send zeros.
        f.color = Default::default();
        f.color.contrast = 0.5;
        let dark = solid(1, 1, [0, 0, 0, 255]);
        assert!(map_pixels(&f, &dark, 1, 1).iter().all(|&v| v > 0));
        let d = &decode_all(&frame_packets(&[f], 0, &dark, (1, 1), 1, true))[0];
        assert!(d.data.iter().all(|&v| v == 0));
    }

    #[test]
    fn universes_used_matches_the_splitter() {
        for sc in [1u16, 2, 11, 509, 510, 511, 512] {
            for cpp in [1usize, 3, 4] {
                for n in [0usize, 1, 3, 4, 100, 600, 1200] {
                    let n = n / cpp * cpp;
                    let data = vec![0u8; n];
                    assert_eq!(
                        universes_used(sc, n, cpp),
                        split_universes(0, sc, &data, cpp).len(),
                        "sc {sc} cpp {cpp} n {n}"
                    );
                }
            }
        }
    }

    #[test]
    fn port_address_splits_into_net_sub_universe() {
        assert_eq!(split_port(0x1234), (0x12, 3, 4));
        assert_eq!(join_port(0x12, 3, 4), 0x1234);
        assert_eq!(join_port(0xFF, 0xFF, 0xFF), MAX_UNIVERSE);
        assert_eq!(
            join_port(
                split_port(0x7FFF).0,
                split_port(0x7FFF).1,
                split_port(0x7FFF).2
            ),
            0x7FFF
        );
    }

    #[test]
    fn poll_reply_parses_from_a_real_layout() {
        let r = PollReply {
            ip: [192, 168, 1, 50],
            port: PORT,
            net: 1,
            sub: 2,
            short_name: "Pixie".into(),
            long_name: "Pixie LED controller".into(),
            num_ports: 4,
            mac: [1, 2, 3, 4, 5, 6],
        };
        match decode(&encode_poll_reply(&r)) {
            Some(Packet::PollReply(g)) => assert_eq!(g, r),
            o => panic!("{o:?}"),
        }
        // Negative control: a truncated reply is refused.
        assert!(decode(&encode_poll_reply(&r)[..200]).is_none());
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
            effect: None,
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

    fn slice_at(x0: f32, y0: f32, x1: f32, y1: f32) -> vjc_model::Slice {
        let mut s = crate::mapping::new_slice(400, 200);
        s.output_mesh = vec![(x0, y0), (x1, y0), (x0, y1), (x1, y1)];
        s.mesh_cols = 2;
        s
    }

    #[test]
    fn fixture_follows_a_moved_slice_within_one_pixel() {
        let mut s = slice_at(100.0, 50.0, 300.0, 150.0);
        let f = Fixture {
            input_slice: Some(s.id),
            ..Fixture::default()
        };
        let (i, dangling) = resolve_input(&f, std::slice::from_ref(&s), 400, 200);
        assert!(!dangling);
        assert!((i.x - 0.5).abs() < 1.0 / 400.0 && (i.w - 0.5).abs() < 1.0 / 400.0);
        // Move the slice by 40 x 20 px: the fixture rect follows.
        for p in &mut s.output_mesh {
            p.0 += 40.0;
            p.1 += 20.0;
        }
        let (i, _) = resolve_input(&f, std::slice::from_ref(&s), 400, 200);
        assert!((i.x * 400.0 - 240.0).abs() <= 1.0, "x {}", i.x * 400.0);
        assert!((i.y * 200.0 - 120.0).abs() <= 1.0, "y {}", i.y * 200.0);
        assert!((i.w * 400.0 - 200.0).abs() <= 1.0 && (i.h * 200.0 - 100.0).abs() <= 1.0);
    }

    #[test]
    fn dangling_input_slice_falls_back_to_the_fixture_rect() {
        let s = slice_at(100.0, 50.0, 300.0, 150.0);
        let mut f = Fixture {
            input_slice: Some(uuid::Uuid::new_v4()),
            ..Fixture::default()
        };
        f.input.x = 0.25;
        let (i, dangling) = resolve_input(&f, &[s.clone()], 400, 200);
        assert!(dangling);
        assert_eq!(i, f.input);
        // No link: the rect as is, not dangling.
        f.input_slice = None;
        assert_eq!(resolve_input(&f, &[s], 400, 200), (f.input, false));
    }

    #[test]
    fn resolve_fixtures_writes_the_followed_rect() {
        let s = slice_at(100.0, 50.0, 300.0, 150.0);
        let f = Fixture {
            input_slice: Some(s.id),
            ..Fixture::default()
        };
        let out = resolve_fixtures(&[f.clone()], &[s], 400, 200);
        assert_eq!(out[0].input.w, 0.5);
        assert_eq!(out[0].input_slice, f.input_slice);
    }
}
