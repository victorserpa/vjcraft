# Sync with DJ players
Issue: #30 · Status: draft · Estimate: 1 day + a session with real players

## Problem
VJs want clips to follow the DJ's tempo and track position.

## Goals / non-goals
- Goal: read BPM, beat phase and track position from players on the LAN using the publicly documented community protocol notes (no vendor SDK), and expose them as a tempo source and a clip transport mode.
- Non-goal: sending control to players, metadata databases.

## Design
- Network listener on a chosen interface (pref); passive where the protocol allows.
- Tempo source plugs into the existing clock (like Ableton Link); clip transport "follow player N".

## Test plan (written before the code)
- Replay captured packets (recorded by the owner on real gear, stored as test fixtures) and check BPM/phase decoding.
- Clock follows a synthetic tempo stream with < 5 ms phase error.

## Acceptance
With a real player on the LAN, the master tempo follows within one beat and a clip follows the track.

## Risks and open questions
- Needs real hardware to record fixtures and verify. Protocol changes between firmware versions.
