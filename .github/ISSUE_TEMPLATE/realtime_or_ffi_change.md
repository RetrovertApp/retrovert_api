---
name: Real-time or FFI change
about: Change decode, visualization, settings, metadata, loader, I/O, or an FFI seam
title: ""
labels: ""
assignees: ""
---

## Outcome

<!-- Describe caller-visible behavior, not an implementation task list. -->

## Complete call path

<!-- Entry point -> host modules -> FFI callbacks/adapters -> returned data. -->

## Cadence and budget

| Phase | Calls | Maximum storage/work | Allocations | Locks | Callbacks/adapters |
|---|---|---|---|---|---|
| Setup/open | | | | | |
| Per read/frame | | | zero after preparation | | |
| Control/shutdown | | | | | |

## Ownership and hostile input

<!-- Identify library-, host-, session-, and frame-owned state. List every external count,
dimension, enum, pointer, and length plus its validation cap. -->

## Acceptance evidence

- [ ] Correctness is tested through the affected module's interface.
- [ ] Steady-state allocations, callback counts, and buffer capacities are asserted.
- [ ] Invalid and maximum plugin-controlled values are tested.
- [ ] Lifecycle cleanup and session isolation are tested when applicable.
- [ ] Common paths have an explicit copy count or benchmark when applicable.

See `docs/runtime-contract.md` for the required contract.
