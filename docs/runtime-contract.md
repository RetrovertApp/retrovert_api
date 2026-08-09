# Runtime and FFI Contract

**Status:** Required for new code and changes to existing code  
**Applies to:** the Rust host, generated C interface, and plugin implementations

This document defines performance, ownership, and adversarial-input behavior that is part
of a module's interface even when it is not visible in a Rust type or C declaration.
Correct output alone is not sufficient for code on a real-time path.

## Cadences

Every operation must belong to one cadence. An issue or pull request which adds a call
must name its cadence.

| Cadence | Examples | Heap allocation | Locks | I/O |
|---|---|---|---|---|
| Setup | load, create, open, layout query, buffer preparation | bounded and fallible | allowed | allowed |
| Steady-state audio | `read_data`, conversion, adaptation, resampling | none after preparation | no blocking locks | none |
| Steady-state visualization | position, cells, scope and VU capture | none after preparation | no blocking locks | none |
| Control | settings edits, enable/disable, metadata retrieval | bounded | allowed briefly | allowed outside locks |
| Shutdown | close, destroy, static destroy | bounded | allowed | allowed |

Audio is explicitly prepared by consuming an open `Player` with
`Player::prepare(max_frames)`. Only the resulting `PreparedPlayer` exposes `read`, and it
rejects requests above that fixed budget before calling the plugin. Preparation reserves
bounded worst-case storage because the ABI reveals the native sample format only on the
first read. Visualization is explicitly prepared by `Player::prepare_visualization` and
`VizLayout::new_snapshot`.

"No allocation" means no allocation, reallocation, capacity growth, collection cloning,
or construction of an owned temporary by host code. A callback on a steady-state cadence
must offer the same guarantee in its plugin implementation.

## Real-time rules

- Allocate and validate maximum storage during setup. Refill caller-owned storage in
  place on every steady-state call.
- Match formats, capabilities, and strategies once during setup. Do not dispatch on an
  invariant format for every sample.
- Common passthrough formats must not travel through redundant intermediate copies.
- Work per call must be bounded by a validated caller budget and validated plugin
  dimensions.
- Do not perform filesystem access, network access, persistence, logging which formats or
  allocates, or other unbounded work on a steady-state path.
- Do not hold a host lock while invoking a plugin callback or an injected adapter.
- Do not use a blocking lock on a steady-state path. State shared with a control thread
  must be published as an immutable snapshot or transferred through bounded storage.
- Callback cadence is part of the interface. Structure and layout callbacks run during
  setup; only explicitly documented live callbacks run per frame.

## FFI and ownership rules

- Treat every count, enum discriminant, pointer, returned length, dimension, and status
  supplied by a plugin as hostile input.
- Validate and cap raw counts before integer conversion, multiplication, slicing,
  allocation, decoding strings, or iteration. Use checked arithmetic for dimensions.
- A returned count may never exceed the capacity supplied to a callback. Exact-fill
  queries must also reject underfill.
- Reject a descriptor before calling `create` or `open` when callbacks required for its
  complete lifecycle are absent. A successfully created resource must have one guaranteed
  destruction path.
- State is scoped to the narrowest owner that gives it meaning: library-global state on
  the loaded plugin, song state on the session, and frame state on reusable snapshots.
  Global sharing must be an explicit interface decision.
- No panic or unwind may cross an FFI seam. Pointers returned to a plugin must have a
  documented lifetime and stable address for that lifetime.

## Required verification

Changes must add or update tests at the affected module's interface. Depending on the
change, cover the applicable items below:

- zero allocations on the second and subsequent steady-state call;
- stable addresses and capacities of prepared buffers;
- setup callback counts and permitted per-frame callback counts;
- maximum, zero, overflowing, underfilled, and overfilled plugin dimensions;
- descriptor rejection for every missing required lifecycle callback;
- isolation between two simultaneous sessions;
- no adapter or plugin callback while a host lock is held;
- output equivalence for specialized fast paths;
- a benchmark when copy volume or work per sample/frame can regress materially.

The allocation tests use a thread-local counter around deliberately allocation-free plugin
fixtures. This isolates host allocations from parallel test-runner activity. `cargo test`,
Clippy, formatting, sanitizers, and ABI parity are complementary checks; none replaces the
runtime assertions above.

## Change review

Before implementation, fill in the cadence table in the issue. During review, walk the
complete caller-to-plugin path and answer:

1. What allocates, at which cadence, and with what maximum?
2. What locks, and which thread can contend for it?
3. Can plugin or adapter code run while a host lock is held?
4. Which external values control allocation size or loop work?
5. Is each piece of state library-, host-, session-, or frame-owned?
6. Which callbacks run once, and which run per sample, block, or frame?
7. How many full-buffer copies occur on each common path?

Review evidence should cite a test, benchmark, trace, or complete call-path inspection.
Passing formatting and lint checks is not evidence of real-time safety.
