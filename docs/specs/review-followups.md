# Review follow-ups
Issue: #22 · Status: draft · Estimate: 2-3 h in batches

## Problem
`/objection` left MEDIUM and LOW findings open on each PR; #22 collects them. Thirteen were fixed in #31.

## Plan
Batch by area, one PR each, each fix with a test and a negative control:
1. Reliability: OOM kill vs Force Quit (distinguish via exit reason where the OS gives it), OUTPUT_LIVE reset, rehearsal gate on auto-recover, crash writer pruning, reaper serialisation.
2. Output window: drop position, inset_mask, guide image decode off the UI thread, multi-resize, narrow canvas, row spacing cache.
3. Media/HAP: clip snapshot of HAP clips, queued-space double count, .part cleanup across instances.
4. UI polish: zebra contrast, Multiple slices header band, Mask band checkbox, combos outside prop_row.
5. Tests that skip without a GPU: add a CPU reference path where possible.

## Acceptance
Every item in #22 is fixed or explicitly closed with a reason.
