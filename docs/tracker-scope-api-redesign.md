# Tracker / Scope Visualization API — Redesign

**Status:** Design agreed, not yet implemented
**Date:** 2026-06-21
**Scope:** The tracker (pattern) and scope (waveform) visualization surface of the playback plugin API (`retrovert_api/include/retrovert/playback.h`).
**Decision record:** captured from a design (grill) session; this document is the source spec for the `api_generator` `.def` changes.

---

## 1. Motivation

The current visualization API (`RVTrackerInfo`, `get_tracker_info`, `get_pattern_cell`, `get_pattern_num_rows`, `get_scope_data`, `get_scope_channel_names`) is used in an "undefined" way: the struct carries several incompatible data models at once, several fields are unused or contradictory, and the frontend works around the ambiguity with hardcoded per-format logic. We are about to build a new UI (on `em_ui`/Flowi), nothing is shipped, and the API is at an inflection point — so we redesign rather than patch.

### Concrete problems in the current API

- **`RVTrackerInfo` mixes 3+ data models** in one struct:
  - pattern / random-access (MOD/XM/IT) — `num_patterns`, `current_pattern`, `rows_per_pattern`, `get_pattern_cell(pattern,row,channel)`
  - channel-based / per-channel scrolling (TFMX) — `total_rows`, `channels[].num_rows`, `channels[].current_row`, `channels_synchronized`
  - synthesized-from-event-stream (VGM) — `current_sample`, `void* native_pattern_data` (an opaque escape hatch pointing at a plugin-private `VgmPattern*`).
- **`native_pattern_data` is read by no consumer** — pure dead weight + abstraction leak.
- **`channels_synchronized` is ignored** by the frontend, which hardcodes a `VGM/TFMX/SID` format list to choose scrolling mode instead.
- **`RV_MAX_CHANNELS = 8` is fiction** — openmpt/hively exceed it with no bounds check; the frontend uses 64; only tfmx clamps.
- **Static and dynamic data are conflated.** `get_tracker_info` returns a 1208-byte struct mixing immutable names (`song_name`, `sample_names[32][24]`) with live position (`current_row`), and the frontend polls all of it ~500×/second.
- **Effect encoding is inconsistent** across plugins: openmpt writes an ASCII effect *letter*, hively a raw numeric code, tfmx a letter-or-command depending on an internal row type. The frontend cannot render effects generically.
- **`note` range is under-defined** (`1-120`, `255`=off; `121-254` undefined; no distinct cut/fade/release).
- **Scope capture has a hidden side effect**: the first `get_scope_data` call silently enables capture forever, with no off switch.
- **Channel-name lifetime is fragile**: plugin-owned pointers "valid until close," re-copied by the frontend every frame.
- **Per-cell access**: `get_pattern_cell` is one FFI call per cell — up to ~4096 calls per cache refresh.
- **Header duplication**: consumers vendor their own copy of `playback.h`.

---

## 2. Current-state taxonomy (what plugins actually do)

### Cell-providing plugins

| Plugin | Data model | Scrolling | Notes |
|---|---|---|---|
| `openmpt` (MOD/XM/S3M/IT) | Native, random-access | synchronized | full module in memory |
| `hively` (AHX) | Position→track indirection, random-access | synchronized | no `memset`, no channel bound check |
| `tfmx` | Channel-based, decoder-computed | **per-channel** (`channels_synchronized=0`) | `pattern` arg ignored; only user of `dest_channel`; note-row vs command-row duality |
| `libvgm` (VGM) | Synthesized from register stream **at open** → full timeline | **per-channel** | only user of `native_pattern_data` (unread) |
| `sidplayfp` | **metadata only** | — | `get_pattern_cell` = NULL |

Key insight: every cell-provider actually has the full structure available **after `open()`** (even libvgm, which parses the whole stream up front). We nonetheless do **not** bake that in as an invariant (future formats unpredictable) — see Decision 2.

### Scope

Implemented by `libvgm`, `sidplayfp`, `uade`, `xsf` (PSF only), `organya`, `spu`, `adplug`, `v2m`, `pxtone`. Per-voice float tap from the emulator/synth. Channel counts fixed (SID=3, Paula=4, Organya=16, SPU=1) or dynamic (v2m≤64, xsf≤48, pxtone≤32). All share the same "first call auto-enables capture" side effect.

### No visualization at all

`furnace`, `klystrack`, `sc68`, `mdxmini`, `pmdmini`, `gme`, `asap`, `sunvox`.

---

## 3. Design principles

1. **Capability-driven, not host-hardcoded.** Plugins advertise what they support; the host never special-cases a format name.
2. **Honest about provenance.** Tracker data (known up front) and event-stream data (synthesized) differ; the API models the differences that matter (scrolling model, valid window, cell semantics) rather than pretending they're identical or forcing one model.
3. **Format-specific displays stay possible.** A generic renderer must work for any plugin from declared metadata, but bespoke per-format renderers (ProTracker view, VGM view) remain first-class.
4. **One source of truth per concern.** Catalog text lives in the metadata API; typed render structure lives in the tracker query; position has one clock.
5. **Host owns all concurrency.** Plugins are single-threaded and lock-free.

---

## 4. Decisions

### 4.1 Single capability-driven API
One set of functions. The plugin advertises a **capability bitset** plus a **scrolling mode**. The host reads capabilities; it does **not** branch on format name. This directly removes the frontend's hardcoded `VGM/TFMX/SID` list and its reliance on (currently ignored) `channels_synchronized`.

```
capabilities (bitset): PATTERN_CELLS, SCOPE, VU, WHOLE_SONG_KNOWN,
                       SEEKABLE_PREVIEW, FUTURE_KNOWN, ...
scrolling_mode: SYNCHRONIZED | PER_CHANNEL
```

### 4.2 No "whole song known" invariant — windowed baseline
Baseline access is a **window** around the current position. A plugin reports the row range that is **valid to query right now**:
- random-access tracker → window is the whole pattern/song (advertises `WHOLE_SONG_KNOWN`);
- live/streaming source → window is `[current−N, current+M]`.

The richer guarantees are capability flags, not assumptions. The frontend's existing ±N-row cache becomes the *contract*, not a hack.

### 4.3 Split queries by cadence
Replace the single fat per-frame struct with queries grouped by how often they change:

| Query | Cadence | Carries |
|---|---|---|
| **Structure / capabilities** | once at open | capability bitset, scrolling mode, channel count + names, num patterns/orders, cell schema |
| **Live position** | per frame (small) | current order/pattern/row, per-channel current rows, valid window range, output-frame timestamp |
| **Cells** | on demand, windowed | the pattern cell block |
| **Scope** | per frame | per-channel waveform |

Position split: **plugin owns musical position** (order/pattern/row — only it knows the format); **core owns wall-clock / sample position** (seek bar, elapsed time). One clock each, no duplication.

### 4.4 Catalog/text → metadata API (+ read API)
Immutable, human-readable, per-URL catalog data moves to the existing metadata API (which already carries title/artist/date/length/subsongs and `add_sample`/`add_instrument`): **title, artist, album, sample names, instrument names, length, subsongs, module type.** Delete `song_name`/`sample_names[32][24]` etc. from the tracker struct (also kills the 32×24 caps).

This requires a new **metadata read API** for the UI side (today the metadata API is write-only from the plugin). The frontend reads names from the core's metadata store by URL/index — one source of truth. The browser/playlist need this read API anyway.

The tracker structure query keeps only **typed structural** data (channels, scrolling mode, capabilities, counts). No stringly-typing structure into metadata tags.

### 4.5 Delete `native_pattern_data`
Dead — no consumer reads it. Removed entirely.

### 4.6 Independent channel sets; dynamic counts; caller-owned names
**Pattern channels and scope channels are independent**, each owned by its capability with its own dynamic count and names. (A plugin may have scope-but-no-pattern, e.g. adplug; or more scope voices than tracker tracks.) When they correspond (the common case), the plugin reports matching counts/names so the UI can align them.
- `RV_MAX_CHANNELS = 8` removed; counts are dynamic `u32`.
- Channel-name strings: **caller-provides-buffer**, queried **once** with the structure (not per frame). No dangling-pointer-until-close assumptions.

### 4.7 Schema-driven cells, plugin owns formatting
Each plugin declares its **column schema once** at open: an ordered list of columns, each `{label, char_width, kind}`. Because tracker columns are fixed-width by nature (note = 3 chars, sample = 2, effect = 1+param…), the plugin **formats its own columns into fixed-width text fields**. Per cell, fixed-size record carries, **per column**:
- the **plugin-rendered fixed-width text** (what gets drawn), and
- the **raw value(s)** (so the frontend can color/highlight — note-on tint, TFMX `dest_channel` routing — without parsing text).

The frontend lays out the pre-rendered text generically; bespoke per-format renderers remain possible. This puts format knowledge where it lives (the plugin), kills the inconsistent-effect-encoding problem, and keeps cells fixed-size.

### 4.8 `get_cells()` batch fetch
Replace per-cell access with a block fetch over a row range, bounded by the valid window, into a fixed-stride caller buffer.

```
// fills out_buffer with cells for the requested block; returns count actually filled
uint32_t get_cells(void* user_data,
                   int channel /* or ALL for synchronized */,
                   int row_lo, int row_hi,
                   RVCell* out_buffer);
```

- Caller knows `cell_size` from the schema → allocates `cell_size × rows × channels`.
- **Returns the count actually filled** (short at song end or at the edge of a stream's known window).

### 4.9 Scope contract
- **Explicit `set_scope_enabled(on)`** gated by the `SCOPE` capability. No hidden auto-on; there is an off switch so capture cost is only paid while visualizing.
- **VU is a separate optional capability** (`VU`): a cheap per-channel peak/RMS query that does **not** require full scope capture. Frontend derives VU from scope as a fallback when only `SCOPE` is present.
- `get_scope_data` copies the most recent ≤N frames into a **caller buffer** (plugin handles its own internal capture-buffer locking; the frontend never holds pointers into plugin/audio-thread memory), float `[-1,1]`, at the playback sample rate. Returns frame count filled.

### 4.10 Stereo scope
Scope channels may be stereo (software synths, stereo samples).
- Each scope channel declares **width: 1 (mono) or 2 (stereo)** in the structure query. Per-channel (a song can mix mono chip voices with a stereo synth voice).
- `get_scope_data` returns **interleaved** samples at that width; caller sizes the buffer from the declared width; returned count is in frames.
- The **frontend downmixes to mono by default** (basic oscilloscope keeps working unchanged) and can draw dual-trace / Lissajous / stereo VU when desired. No mix-to-mono in the plugin (lossy, and it's the frontend's call).

### 4.11 Threading — host owns it entirely
The **core calls every visualization getter on the decode thread** (interleaved with `read_data`, the same thread the decoder already runs on), copies the results into a coherent **snapshot**, and hands that to the UI thread safely. Plugins stay single-threaded and lock-free — thread-safety is solved by *where* the call happens, not by plugin-side locking.

The snapshot is stamped with the **output-frame position** so position, cells, and scope all reference the same instant, and that instant can be aligned to **output presentation time** (what is being heard) rather than decode time. The timestamp is part of the contract from day one so sample-accurate sync is *possible*, even if the first implementation samples approximately. The frontend keeps a double/triple buffer purely for render smoothness; its source is the coherent core snapshot.

---

## 5. Proposed API shape (sketch)

Illustrative only; the authoritative definition is the `api_generator` `.def`.

```c
// ---- capabilities ----
typedef enum RVVizCaps {
    RVViz_PatternCells   = 1 << 0,
    RVViz_Scope          = 1 << 1,
    RVViz_Vu             = 1 << 2,
    RVViz_WholeSongKnown = 1 << 3,
    RVViz_SeekablePreview= 1 << 4,
    RVViz_FutureKnown    = 1 << 5,
} RVVizCaps;

typedef enum RVScrollMode { RVScroll_Synchronized = 0, RVScroll_PerChannel = 1 } RVScrollMode;

typedef enum RVColumnKind { RVCol_Note, RVCol_Instrument, RVCol_Volume, RVCol_Effect, RVCol_Param, RVCol_Custom } RVColumnKind;

typedef struct RVColumnDesc {
    char label[16];
    uint8_t char_width;     // fixed-width text field for this column
    RVColumnKind kind;      // drives default coloring
} RVColumnDesc;

// ---- structure: queried ONCE at open ----
typedef struct RVVizStructure {
    uint32_t caps;              // RVVizCaps bitset
    RVScrollMode scroll_mode;
    uint32_t pattern_channel_count;
    uint32_t scope_channel_count;
    uint32_t column_count;      // length of the cell schema
    // names + per-scope-channel width fetched via dedicated calls into caller buffers
} RVVizStructure;

// ---- live position: queried per frame ----
typedef struct RVVizPosition {
    uint64_t output_frame;      // single clock; alignable to playback/output time
    uint32_t current_order, current_pattern, current_row;
    uint32_t window_lo, window_hi; // currently-valid row range
    // per-channel current rows fetched into a caller buffer for PER_CHANNEL mode
} RVVizPosition;

// ---- one cell (fixed size; size derived from schema) ----
typedef struct RVCell {
    // per column: raw value (logic/color) + fixed-width rendered text (display)
    // laid out per the schema; see api_generator output for exact packing
    uint8_t  note;            // typed, universal (color/highlight)
    uint8_t  instrument;      // typed, universal
    // columns[]: { uint32_t raw; char text[char_width]; } packed per RVColumnDesc
} RVCell;

typedef struct RVPlaybackVizApi {
    int      (*get_structure)(void* ud, RVVizStructure* out);
    uint32_t (*get_column_schema)(void* ud, RVColumnDesc* out, uint32_t max);
    uint32_t (*get_pattern_channel_names)(void* ud, char* out, uint32_t name_stride, uint32_t max);
    uint32_t (*get_scope_channel_names)(void* ud, char* out, uint32_t name_stride, uint32_t max);
    uint32_t (*get_scope_channel_widths)(void* ud, uint8_t* out, uint32_t max);

    int      (*get_position)(void* ud, RVVizPosition* out);
    uint32_t (*get_cells)(void* ud, int channel, int row_lo, int row_hi, RVCell* out);

    void     (*set_scope_enabled)(void* ud, int on);
    uint32_t (*get_scope_data)(void* ud, int channel, float* out, uint32_t max_frames); // interleaved by width
    uint32_t (*get_vu)(void* ud, float* out_peak, uint32_t max_channels);               // optional (RVViz_Vu)
} RVPlaybackVizApi;
```

---

## 6. Migration plan (staged)

1. **Codegen-first.** Define the new API in `api_generator` `.def`; regenerate the C headers (`retrovert_api`) **and** the Rust core bindings (`retrovert-core`). This also clears the standing **v1 → v2 plugin-API drift** between the 2024 Rust core and the 2026 C plugins.
2. **Proving-ground plugins** (one per model):
   - `openmpt` — synchronized random-access
   - `tfmx` — per-channel scrolling
   - `libvgm` — synthesized + windowed; delete `native_pattern_data`
   - `sidplayfp` — metadata-only + scope
   - `v2m` or `pxtone` — stereo scope + VU capability
3. **Migrate the 3 consumers** off the old API: drop the hardcoded format list, honor advertised scrolling mode + window, drop the `8`/`64` magic, depend on `retrovert_api` headers directly (stop vendoring `playback.h`).
4. **Mechanical rollout** to remaining plugins; then **delete** dead fields (`native_pattern_data`, tracker-side names, dual position source).

### Consumers (must keep working through migration)
- `replay_frontend/src/plugins/ui/music_player/` — primary
- `replay_vamiga_hle_rom/src/plugins/ui/music_player/` — a near-**duplicate** of the same music_player (candidate to become a single shared component)
- `retrovert_console/src/main.c` — C console

All three currently vendor their own copy of `playback.h`; consolidate to a single source.

---

## 7. Open items

- Exact packing of `RVCell` columns (raw + fixed-width text) in the generated layout.
- Whether the metadata **read** API is part of this work or a parallel track (needed by browser/playlist regardless).
- Whether to collapse the duplicated `music_player` into one shared component before or after the API migration.
- Precision target/implementation for output-presentation-time sync (designed-in now; tightened later).
