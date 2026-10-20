# Sync with DJ players
Issue: #30 · Status: implemented against synthetic packets (no real hardware yet) · Estimate: 1 day + a session with real players

## Problem
VJs want clips to follow the DJ's tempo and track position.

## Goals / non-goals
- Goal: read BPM, beat phase and track position from players on the LAN using the publicly documented community protocol notes (no vendor SDK), and expose them as a tempo source and a clip transport mode.
- Non-goal: sending control to players, metadata databases.

## Design
- Network listener on a chosen interface (pref); passive where the protocol allows.
- Tempo source plugs into the existing clock (like Ableton Link); clip transport "follow player N".

## Test plan (written before the code)
- Replay packets and check BPM/phase decoding. Done with synthetic packets built from the documented layout (`djlink::encode_*`) and a loopback UDP simulator; the originally planned recorded fixtures still need real gear.
- Clock follows a synthetic tempo stream with < 5 ms phase error.

## Acceptance
With a real player on the LAN, the master tempo follows within one beat and a clip follows the track.

## Risks and open questions
- Needs real hardware to record fixtures and verify. Protocol changes between firmware versions.

## Implementation notes
- `vjc-core/src/djlink.rs`: decode/encode, tracker (master = flagged player, else lowest live), follower steering the `Clock` (tempo with pitch, bar phase). `vjc-app/src/dj_io.rs`: listener thread. Prefs tab "DJ sync"; status next to the Link button.
- Passive and read-only: beat packets (UDP 50001) give tempo, beat in bar and phase without announcing a device. Status packets (50002) carry the master flag and beat counter, but players may only send them to announced devices; announcing a virtual device is not implemented (it would have to be opt-in). Without status, master = lowest-numbered live player, or pick the player in Prefs.
- Off by default; listens only on the interface IP chosen (bad value falls back to 127.0.0.1). Link, when on, wins over DJ sync.
- Clip transport "follow player N": see dj-follow-player.md (needs status packets). Not done: Linux/Windows broadcast reception on a specific interface and sharing the ports with other software are unverified.
