# Specs

Every non-trivial change goes through four steps, in order:

1. **Brainstorm**: the problem, who hits it, two or three ways to solve it, the pick and why.
2. **Spec**: a file in this folder (template below), reviewed in its own PR before code.
3. **Tests first**: the tests from the spec's test plan are written and fail.
4. **Build**: implement until they pass, then `/objection`, then merge.

A spec PR touches only `docs/specs/`. The implementation PR links its spec and ticks its acceptance list.

## Template

```
# <title>
Issue: #N · Status: draft | approved | done · Estimate: <hours>

## Problem
## Goals / non-goals
## Design
## Test plan (written before the code)
## Acceptance
## Risks and open questions
```

## Index

| spec | issue | estimate | needs hardware |
| --- | --- | --- | --- |
| [hw-decode.md](hw-decode.md) | new | 1.5-2 h | no |
| [ffmpeg-hap-build.md](ffmpeg-hap-build.md) | new | 0.5-1 h | no |
| [cross-platform-builds.md](cross-platform-builds.md) | #15 | 2-3 h + owner test | Windows PC |
| [releases.md](releases.md) | new | 1-2 h | no |
| [led-wall-test.md](led-wall-test.md) | new | owner session | LED 3x2 |
| [slices-advanced.md](slices-advanced.md) | #7 | 3-4 h | projector/LED to verify |
| [review-followups.md](review-followups.md) | #22 | 2-3 h | no |
| [ffgl-host.md](ffgl-host.md) | #28 | ~1 day | no |
| [audio-plugins.md](audio-plugins.md) | #29 | ~1 day | no |
| [dj-sync.md](dj-sync.md) | #30 | 1 day + test | CDJ/XDJ or Denon player |
| [ten-bit-output.md](ten-bit-output.md) | #11 | 0.5-1 day | 10-bit display |
