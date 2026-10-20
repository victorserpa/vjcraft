# Clip follows a DJ player

Issue: #30 · Status: approved · Estimate: 1 day, plus a session with real players

## Problem

DJ sync drives the master tempo, but a clip cannot follow one player's track: its play state, pause, cue and position.

## Goals and non-goals

- Goal: a per-clip transport option, "Follow player 1–4", that tracks the player's play state and track position, scaled by pitch.
- Non-goals:
  - waveforms or track metadata;
  - sending control to players;
  - tracks with variable tempo (position is derived from beats).

## Behaviour

- The clip Transport panel gets "Follow player": Off, 1, 2, 3 or 4. The default is Off.
- **Player playing:** the clip plays at clip speed × pitch. With "Master tempo" on, it plays at clip speed and still holds position.
- **Player paused:** the clip pauses on its current frame.
- **Cue or jump** (a position change larger than one beat in one update): the clip jumps on the next frame.
- **Mapping:** clip time = in point + (track seconds mod clip length). A clip shorter than the track loops.
- **Player lost** (silent past the stale time) or **no beat counter** (no status packets): the clip runs free at its last rate. The panel says "Player N lost, running free" or "Player N sends no position yet". The clip locks again when data comes back.
- While following, trigger, direction and play mode are ignored. They apply again when the option is set back to Off.

## What is decoded today (crates/vjc-core/src/djlink.rs)

There is no position in ms. The only position-like value is `Status.beat_count`. Status packets also carry `playing`, `bpm`, `pitch` and `beat_in_bar`. Beat packets carry `bpm` and `pitch` but no play state.

Status packets may not reach a passive listener (see dj-sync.md). Without them, a followed clip stays free and shows the note.

## Data model

`Transport` gets two new fields:
- `#[serde(default)] follow_player: Option<u8>` (1..=4)
- `#[serde(default)] follow_master_tempo: bool`

Old projects load with follow off. `PlayMode` is unchanged.

## Design

- **`Tracker`** keeps the last `playing`, `beat_count`, `bpm`, `pitch` and receive instant for each player. It exposes `track_pos(player, now) -> Option<TrackPos { secs, rate, playing }>`, where:
  - `secs = beat_count × 60 / bpm`;
  - while playing, `secs` advances by `elapsed × rate` since the last packet.
- **`ClipFollower { pos, rate }`** is a pure type in vjc-core. Its `step(target, dt, clip_len)` works as follows:
  - drift = target − pos, wrapped to the clip length;
  - |drift| > 40 ms → hard lock;
  - otherwise rate = target.rate × (1 + clamp(drift × 0.5 / s, ±5 %)). This gives no visible stutter, and the drift closes in about 1 s;
  - no target → keep pos and rate (free run);
  - not playing → rate 0.
- **App:** where clips advance, a clip with `follow_player` set uses `ClipFollower::step` instead of the normal advance. Each clip keeps one follower in runtime state; it is not saved.

## Tests (written first, synthetic packets only)

- **Decode:** round trip of `playing`, `beat_count` and `pitch` for players 1–4.
- **`track_pos`:**
  - 64 beats at 128 BPM gives 30.0 s;
  - it extrapolates while playing and holds while paused.
- **Jitter:** with ±10 ms packet jitter there is no hard lock, and |drift| stays under 20 ms for 60 s.
- **Pitch:** +8 % pitch gives rate 1.08; with master tempo the rate is 1.0 and position is still held.
- **Jumps and pause:** a 16-beat jump locks within 1 step; a pause gives rate 0 in the same step.
- **Mapping:** a 10 s clip with the track at 25 s sits at 5 s.
- **Stale player:** free run, then a relock when packets resume.
- **Beat-only stream:** the clip stays free and the note shows.
- **Serde:** an old project loads with follow off.

## Needs real gear

- Whether status packets reach a passive listener.
- The byte offsets on current firmware.
- The real update rate and jitter, to tune the 40 ms threshold and the gain.
- The beat-grid offset from the start of the track.

## Acceptance

With a real player, a followed clip stays within one frame of the track while playing, pauses with it, and jumps on cue.
