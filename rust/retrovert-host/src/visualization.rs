//! Allocation-free steady-state visualization capture beside synchronous decoding.

use core::ffi::c_void;
use std::sync::Arc;

use crate::ffi::playback::{
    RVChannelDesc, RVColumnDesc, RVColumnKind, RVPatternCell, RVPlaybackPlugin, RVScrollMode,
    RVTrackerPosition, RVVizCaps, RVVizInfo,
};

/// Default number of samples reserved per scope channel.
pub const DEFAULT_SCOPE_SAMPLE_BUDGET: u32 = 2_048;
/// Default maximum number of pattern rows captured per frame.
pub const DEFAULT_PATTERN_ROW_BUDGET: u32 = 64;
/// Maximum pattern-channel count accepted from a plugin.
pub const MAX_PATTERN_CHANNELS: u32 = 64;
/// Maximum scope-channel count accepted from a plugin.
pub const MAX_SCOPE_CHANNELS: u32 = 64;
/// Maximum pattern-column count accepted from a plugin.
pub const MAX_PATTERN_COLUMNS: u32 = 16;

/// Fixed storage limits selected before visualization capture starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualizationConfig {
    /// Maximum samples captured for each scope channel.
    pub scope_sample_budget: u32,
    /// Maximum pattern rows captured in one snapshot.
    pub pattern_row_budget: u32,
}

impl Default for VisualizationConfig {
    fn default() -> Self {
        Self {
            scope_sample_budget: DEFAULT_SCOPE_SAMPLE_BUDGET,
            pattern_row_budget: DEFAULT_PATTERN_ROW_BUDGET,
        }
    }
}

/// Immutable visualization structure queried once after a song opens.
#[derive(Debug)]
pub struct VizLayout {
    /// Capability bits reported by the plugin.
    pub caps: u32,
    /// How tracker rows advance across channels.
    pub scroll_mode: RVScrollMode,
    /// Pattern column descriptors.
    pub columns: Box<[RVColumnDesc]>,
    /// Pattern channel descriptors.
    pub pattern_channels: Box<[RVChannelDesc]>,
    /// Scope channel descriptors.
    pub scope_channels: Box<[RVChannelDesc]>,
    config: VisualizationConfig,
    cell_capacity: usize,
}

impl VizLayout {
    /// Allocates one reusable frame slot. Do this during setup, not in the capture loop.
    ///
    /// # Errors
    ///
    /// Returns [`VisualizationError::DimensionOverflow`] when the configured dimensions
    /// overflow, or [`VisualizationError::Allocation`] when a snapshot buffer cannot be reserved.
    pub fn new_snapshot(self: &Arc<Self>) -> Result<VizSnapshot, VisualizationError> {
        let channel_rows = if self.scroll_mode == RVScrollMode::PerChannel {
            boxed_buffer(self.pattern_channels.len(), 0, "tracker_channel_rows")?
        } else {
            Box::default()
        };
        let cells = boxed_buffer(
            self.cell_capacity,
            RVPatternCell {
                raw: 0,
                text: [0; 16],
            },
            "tracker_cells",
        )?;
        let scope_counts = if self.caps & RVVizCaps::SCOPE != 0 {
            boxed_buffer(self.scope_channels.len(), 0, "scope_counts")?
        } else {
            Box::default()
        };
        let scope_capacity = self
            .scope_channels
            .len()
            .checked_mul(self.config.scope_sample_budget as usize)
            .ok_or(VisualizationError::DimensionOverflow)?;
        let scope = if self.caps & RVVizCaps::SCOPE != 0 {
            boxed_buffer(scope_capacity, 0.0, "scope_samples")?
        } else {
            Box::default()
        };
        let vu = if self.caps & RVVizCaps::VU != 0 {
            boxed_buffer(self.scope_channels.len(), 0.0, "vu_levels")?
        } else {
            Box::default()
        };

        Ok(VizSnapshot {
            output_frame: 0,
            layout: Arc::clone(self),
            position: None,
            scope_sample_budget: self.config.scope_sample_budget as usize,
            channel_rows,
            cells,
            cell_count: 0,
            scope,
            scope_counts,
            vu,
        })
    }

    /// Returns the storage limits used to prepare this layout.
    pub const fn config(&self) -> VisualizationConfig {
        self.config
    }
}

/// One reusable, frame-stamped visualization slot.
#[derive(Debug)]
pub struct VizSnapshot {
    /// Output-frame position associated with this snapshot.
    pub output_frame: u64,
    /// Layout that defines all snapshot arrays.
    pub layout: Arc<VizLayout>,
    /// Current tracker position, when the plugin reports one.
    pub position: Option<RVTrackerPosition>,
    scope_sample_budget: usize,
    channel_rows: Box<[u32]>,
    cells: Box<[RVPatternCell]>,
    cell_count: usize,
    scope: Box<[f32]>,
    scope_counts: Box<[u32]>,
    vu: Box<[f32]>,
}

impl VizSnapshot {
    /// Returns per-channel tracker rows for per-channel scrolling layouts.
    pub fn channel_rows(&self) -> &[u32] {
        &self.channel_rows
    }

    /// Returns the populated pattern cells.
    pub fn cells(&self) -> &[RVPatternCell] {
        &self.cells[..self.cell_count]
    }

    /// Returns the populated sample count for each scope channel.
    pub fn scope_counts(&self) -> &[u32] {
        &self.scope_counts
    }

    /// Returns populated scope samples for `channel`, or `None` when it is absent.
    pub fn scope(&self, channel: usize) -> Option<&[f32]> {
        let &count = self.scope_counts.get(channel)?;
        let budget = self.scope_sample_budget;
        let start = channel * budget;
        Some(&self.scope[start..start + count as usize])
    }

    /// Returns VU levels in scope-channel order.
    pub fn vu(&self) -> &[f32] {
        &self.vu
    }
}

/// Why visualization preparation or capture failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum VisualizationError {
    /// The plugin reported a scroll-mode discriminant unknown to this ABI version.
    #[error("scroll mode {0} is unknown")]
    UnknownScrollMode(u32),
    /// A callback reported more values than fit in its output buffer.
    #[error("{query} returned {returned} values for a buffer of {capacity}")]
    CountOverrun {
        /// Callback being validated.
        query: &'static str,
        /// Value count reported by the callback.
        returned: u32,
        /// Capacity supplied to the callback.
        capacity: u32,
    },
    /// A fixed-size callback reported fewer values than requested.
    #[error("{query} returned {returned} values, expected {expected}")]
    CountUnderfill {
        /// Callback being validated.
        query: &'static str,
        /// Value count reported by the callback.
        returned: u32,
        /// Exact value count required.
        expected: u32,
    },
    /// Derived visualization dimensions overflowed their ABI or host representation.
    #[error("visualization dimensions overflow the ABI count")]
    DimensionOverflow,
    /// The plugin reported a tracker window whose upper bound precedes its lower bound.
    #[error("tracker window upper bound {window_hi} precedes lower bound {window_lo}")]
    InvalidTrackerWindow {
        /// Inclusive lower row bound reported by the plugin.
        window_lo: u32,
        /// Exclusive upper row bound reported by the plugin.
        window_hi: u32,
    },
    /// A plugin-reported dimension exceeds the host limit.
    #[error("visualization {dimension} {actual} exceeds the configured limit {limit}")]
    DimensionLimit {
        /// Name of the rejected dimension.
        dimension: &'static str,
        /// Value reported by the plugin.
        actual: u32,
        /// Maximum accepted value.
        limit: u32,
    },
    /// Visualization was prepared earlier with different storage limits.
    #[error("visualization was already prepared with a different configuration")]
    ConfigurationChanged,
    /// Capture was requested before visualization preparation.
    #[error("visualization capture was not prepared")]
    NotPrepared,
    /// The supplied snapshot was allocated for another layout.
    #[error("snapshot belongs to a different visualization layout")]
    LayoutMismatch,
    /// A named snapshot buffer could not be reserved.
    #[error("could not allocate the {0} snapshot buffer")]
    Allocation(&'static str),
}

fn boxed_buffer<T: Copy>(
    count: usize,
    zero: T,
    name: &'static str,
) -> Result<Box<[T]>, VisualizationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| VisualizationError::Allocation(name))?;
    values.resize(count, zero);
    Ok(values.into_boxed_slice())
}

fn read_exact<T: Copy>(
    count: u32,
    zero: T,
    name: &'static str,
    fill: impl FnOnce(*mut T, u32) -> u32,
) -> Result<Box<[T]>, VisualizationError> {
    let mut values = boxed_buffer(count as usize, zero, name)?;
    let returned = fill(values.as_mut_ptr(), count);
    check_exact(name, returned, count)?;
    Ok(values)
}

fn check_exact(name: &'static str, returned: u32, expected: u32) -> Result<(), VisualizationError> {
    if returned > expected {
        return Err(VisualizationError::CountOverrun {
            query: name,
            returned,
            capacity: expected,
        });
    }
    if returned != expected {
        return Err(VisualizationError::CountUnderfill {
            query: name,
            returned,
            expected,
        });
    }
    Ok(())
}

fn check_limit(dimension: &'static str, actual: u32, limit: u32) -> Result<(), VisualizationError> {
    if actual > limit {
        return Err(VisualizationError::DimensionLimit {
            dimension,
            actual,
            limit,
        });
    }
    Ok(())
}

pub(crate) fn prepare_layout(
    plugin: &RVPlaybackPlugin,
    instance: *mut c_void,
    config: VisualizationConfig,
) -> Result<Option<Arc<VizLayout>>, VisualizationError> {
    let Some(viz_info) = plugin.viz_info else {
        return Ok(None);
    };
    let mut info = RVVizInfo {
        caps: 0,
        scroll_mode: RVScrollMode::Synchronized as u32,
        pattern_channel_count: 0,
        scope_channel_count: 0,
        column_count: 0,
    };
    // SAFETY: the instance belongs to this descriptor and `info` is writable for the call.
    if !unsafe { viz_info(instance, &mut info) } {
        return Ok(None);
    }
    let scroll_mode = RVScrollMode::from_raw(info.scroll_mode)
        .ok_or(VisualizationError::UnknownScrollMode(info.scroll_mode))?;
    check_limit(
        "pattern channel count",
        info.pattern_channel_count,
        MAX_PATTERN_CHANNELS,
    )?;
    check_limit(
        "scope channel count",
        info.scope_channel_count,
        MAX_SCOPE_CHANNELS,
    )?;
    check_limit("column count", info.column_count, MAX_PATTERN_COLUMNS)?;

    let cell_capacity = (config.pattern_row_budget as usize)
        .checked_mul(info.pattern_channel_count as usize)
        .and_then(|count| count.checked_mul(info.column_count as usize))
        .ok_or(VisualizationError::DimensionOverflow)?;
    u32::try_from(cell_capacity).map_err(|_| VisualizationError::DimensionOverflow)?;

    let columns = read_exact(
        info.column_count,
        RVColumnDesc {
            label: [0; 16],
            char_width: 0,
            kind: RVColumnKind::Custom as u32,
        },
        "tracker_columns",
        |out, cap| {
            plugin.tracker_columns.map_or(0, |callback| {
                // SAFETY: `out` has `cap` writable entries and the instance is live.
                unsafe { callback(instance, out, cap) }
            })
        },
    )?;
    let channel = RVChannelDesc {
        name: [0; 24],
        scope_width: 0,
    };
    let pattern_channels = read_exact(
        info.pattern_channel_count,
        channel,
        "tracker_channels",
        |out, cap| {
            plugin.tracker_channels.map_or(0, |callback| {
                // SAFETY: `out` has `cap` writable entries and the instance is live.
                unsafe { callback(instance, out, cap) }
            })
        },
    )?;
    let scope_channels = read_exact(
        info.scope_channel_count,
        channel,
        "scope_channels",
        |out, cap| {
            plugin.scope_channels.map_or(0, |callback| {
                // SAFETY: `out` has `cap` writable entries and the instance is live.
                unsafe { callback(instance, out, cap) }
            })
        },
    )?;

    Ok(Some(Arc::new(VizLayout {
        caps: info.caps,
        scroll_mode,
        columns,
        pattern_channels,
        scope_channels,
        config,
        cell_capacity,
    })))
}

pub(crate) fn capture_snapshot(
    plugin: &RVPlaybackPlugin,
    instance: *mut c_void,
    layout: &Arc<VizLayout>,
    output_frame: u64,
    snapshot: &mut VizSnapshot,
) -> Result<(), VisualizationError> {
    if !Arc::ptr_eq(layout, &snapshot.layout) {
        return Err(VisualizationError::LayoutMismatch);
    }

    snapshot.output_frame = output_frame;
    snapshot.cell_count = 0;
    snapshot.scope_counts.fill(0);

    let mut raw_position = RVTrackerPosition {
        order: 0,
        pattern: 0,
        row: 0,
        window_lo: 0,
        window_hi: 0,
    };
    snapshot.position = plugin.tracker_position.and_then(|callback| {
        // SAFETY: `raw_position` is writable and the instance is live.
        unsafe { callback(instance, &mut raw_position) }.then_some(raw_position)
    });

    if layout.scroll_mode == RVScrollMode::PerChannel {
        let expected = layout.pattern_channels.len() as u32;
        let returned = plugin.tracker_channel_rows.map_or(0, |callback| {
            // SAFETY: the snapshot owns `expected` writable row entries.
            unsafe { callback(instance, snapshot.channel_rows.as_mut_ptr(), expected) }
        });
        check_exact("tracker_channel_rows", returned, expected)?;
    }

    if let Some(position) = snapshot.position {
        if layout.caps & RVVizCaps::PATTERN_CELLS != 0 {
            let rows = position.window_hi.checked_sub(position.window_lo).ok_or(
                VisualizationError::InvalidTrackerWindow {
                    window_lo: position.window_lo,
                    window_hi: position.window_hi,
                },
            )?;
            check_limit("pattern row count", rows, layout.config.pattern_row_budget)?;
            let count = (rows as usize)
                .checked_mul(layout.pattern_channels.len())
                .and_then(|count| count.checked_mul(layout.columns.len()))
                .ok_or(VisualizationError::DimensionOverflow)?;
            let capacity =
                u32::try_from(count).map_err(|_| VisualizationError::DimensionOverflow)?;
            let returned = plugin.tracker_cells.map_or(0, |callback| {
                // SAFETY: setup allocated the maximum configured cell count.
                unsafe {
                    callback(
                        instance,
                        -1,
                        position.window_lo,
                        position.window_hi,
                        snapshot.cells.as_mut_ptr(),
                        capacity,
                    )
                }
            });
            check_exact("tracker_cells", returned, capacity)?;
            snapshot.cell_count = count;
        }
    }

    if layout.caps & RVVizCaps::SCOPE != 0 {
        if let Some(scope_samples) = plugin.scope_samples {
            let budget = layout.config.scope_sample_budget as usize;
            for (channel, count) in snapshot.scope_counts.iter_mut().enumerate() {
                let samples = &mut snapshot.scope[channel * budget..][..budget];
                // SAFETY: `samples` holds `scope_sample_budget` writable entries.
                let returned = unsafe {
                    scope_samples(
                        instance,
                        channel as i32,
                        samples.as_mut_ptr(),
                        layout.config.scope_sample_budget,
                    )
                };
                if returned > layout.config.scope_sample_budget {
                    return Err(VisualizationError::CountOverrun {
                        query: "scope_samples",
                        returned,
                        capacity: layout.config.scope_sample_budget,
                    });
                }
                *count = returned;
            }
        }
    }

    if layout.caps & RVVizCaps::VU != 0 {
        let expected = layout.scope_channels.len() as u32;
        let returned = plugin.vu_levels.map_or(0, |callback| {
            // SAFETY: the snapshot owns `expected` writable VU entries.
            unsafe { callback(instance, snapshot.vu.as_mut_ptr(), expected) }
        });
        check_exact("vu_levels", returned, expected)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static INFO_CALLS: Cell<u32> = const { Cell::new(0) };
        static COLUMN_CALLS: Cell<u32> = const { Cell::new(0) };
        static CHANNEL_CALLS: Cell<u32> = const { Cell::new(0) };
    }

    unsafe extern "C" fn info(_instance: *mut c_void, out: *mut RVVizInfo) -> bool {
        INFO_CALLS.set(INFO_CALLS.get() + 1);
        // SAFETY: the test builder supplies a writable `RVVizInfo`.
        unsafe {
            *out = RVVizInfo {
                caps: RVVizCaps::PATTERN_CELLS | RVVizCaps::SCOPE | RVVizCaps::VU,
                scroll_mode: RVScrollMode::Synchronized as u32,
                pattern_channel_count: 2,
                scope_channel_count: 2,
                column_count: 3,
            }
        };
        true
    }

    unsafe extern "C" fn columns(_instance: *mut c_void, out: *mut RVColumnDesc, cap: u32) -> u32 {
        COLUMN_CALLS.set(COLUMN_CALLS.get() + 1);
        let kinds = [
            RVColumnKind::Note,
            RVColumnKind::Volume,
            RVColumnKind::Effect,
        ];
        let count = kinds.len().min(cap as usize);
        for (index, kind) in kinds.into_iter().take(count).enumerate() {
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe {
                *out.add(index) = RVColumnDesc {
                    label: [0; 16],
                    char_width: 3,
                    kind: kind as u32,
                }
            };
        }
        count as u32
    }

    fn named(name: &[u8]) -> RVChannelDesc {
        let mut value = RVChannelDesc {
            name: [0; 24],
            scope_width: 1,
        };
        value.name[..name.len()].copy_from_slice(name);
        value
    }

    unsafe extern "C" fn channels(
        _instance: *mut c_void,
        out: *mut RVChannelDesc,
        cap: u32,
    ) -> u32 {
        CHANNEL_CALLS.set(CHANNEL_CALLS.get() + 1);
        let channels = [named(b"left"), named(b"right")];
        let count = channels.len().min(cap as usize);
        for (index, channel) in channels.into_iter().take(count).enumerate() {
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe { *out.add(index) = channel };
        }
        count as u32
    }

    unsafe extern "C" fn three_channels(
        _instance: *mut c_void,
        out: *mut RVChannelDesc,
        cap: u32,
    ) -> u32 {
        let channels = [named(b"one"), named(b"two"), named(b"three")];
        let count = channels.len().min(cap as usize);
        for (index, channel) in channels.into_iter().take(count).enumerate() {
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe { *out.add(index) = channel };
        }
        count as u32
    }

    unsafe extern "C" fn position(_instance: *mut c_void, out: *mut RVTrackerPosition) -> bool {
        // SAFETY: the test builder supplies a writable position.
        unsafe {
            *out = RVTrackerPosition {
                order: 1,
                pattern: 2,
                row: 4,
                window_lo: 0,
                window_hi: 8,
            }
        };
        true
    }

    unsafe extern "C" fn cells(
        _instance: *mut c_void,
        _channel: i32,
        row_lo: u32,
        row_hi: u32,
        out: *mut RVPatternCell,
        cap: u32,
    ) -> u32 {
        let count = ((row_hi - row_lo) as usize * 2 * 3).min(cap as usize);
        for index in 0..count {
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe {
                *out.add(index) = RVPatternCell {
                    raw: index as u32,
                    text: [index as u8; 16],
                }
            };
        }
        count as u32
    }

    unsafe extern "C" fn scope(
        _instance: *mut c_void,
        channel: i32,
        out: *mut f32,
        cap: u32,
    ) -> u32 {
        for index in 0..cap as usize {
            let value = if channel == 0 {
                index as f32 / cap as f32
            } else {
                0.5
            };
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe { *out.add(index) = value };
        }
        cap
    }

    unsafe extern "C" fn vu(_instance: *mut c_void, out: *mut f32, cap: u32) -> u32 {
        let values = [0.8, 0.3];
        let count = values.len().min(cap as usize);
        for (index, value) in values.into_iter().take(count).enumerate() {
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe { *out.add(index) = value };
        }
        count as u32
    }

    unsafe extern "C" fn overrun_scope(
        _instance: *mut c_void,
        _channel: i32,
        _out: *mut f32,
        cap: u32,
    ) -> u32 {
        cap + 1
    }

    unsafe extern "C" fn underfill_columns(
        _instance: *mut c_void,
        _out: *mut RVColumnDesc,
        cap: u32,
    ) -> u32 {
        cap.saturating_sub(1)
    }

    unsafe extern "C" fn per_channel_info(_instance: *mut c_void, out: *mut RVVizInfo) -> bool {
        // SAFETY: the test builder supplies a writable `RVVizInfo`.
        unsafe {
            *out = RVVizInfo {
                caps: RVVizCaps::PATTERN_CELLS,
                scroll_mode: RVScrollMode::PerChannel as u32,
                pattern_channel_count: 3,
                scope_channel_count: 0,
                column_count: 1,
            }
        };
        true
    }

    unsafe extern "C" fn unknown_scroll_info(_instance: *mut c_void, out: *mut RVVizInfo) -> bool {
        // SAFETY: the test builder supplies a writable `RVVizInfo`.
        unsafe {
            *out = RVVizInfo {
                caps: 0,
                scroll_mode: 99,
                pattern_channel_count: 0,
                scope_channel_count: 0,
                column_count: 0,
            }
        };
        true
    }

    unsafe extern "C" fn overflowing_info(_instance: *mut c_void, out: *mut RVVizInfo) -> bool {
        // SAFETY: the test builder supplies a writable `RVVizInfo`.
        unsafe {
            *out = RVVizInfo {
                caps: RVVizCaps::PATTERN_CELLS,
                scroll_mode: RVScrollMode::Synchronized as u32,
                pattern_channel_count: 65_536,
                scope_channel_count: 0,
                column_count: 65_536,
            }
        };
        true
    }

    unsafe extern "C" fn full_columns(
        _instance: *mut c_void,
        _out: *mut RVColumnDesc,
        cap: u32,
    ) -> u32 {
        cap
    }

    unsafe extern "C" fn full_channels(
        _instance: *mut c_void,
        _out: *mut RVChannelDesc,
        cap: u32,
    ) -> u32 {
        cap
    }

    unsafe extern "C" fn one_row_position(
        _instance: *mut c_void,
        out: *mut RVTrackerPosition,
    ) -> bool {
        // SAFETY: the test builder supplies a writable position.
        unsafe {
            *out = RVTrackerPosition {
                order: 0,
                pattern: 0,
                row: 0,
                window_lo: 0,
                window_hi: 1,
            }
        };
        true
    }

    unsafe extern "C" fn inverted_position(
        _instance: *mut c_void,
        out: *mut RVTrackerPosition,
    ) -> bool {
        // SAFETY: the test builder supplies a writable position.
        unsafe {
            *out = RVTrackerPosition {
                order: 0,
                pattern: 0,
                row: 0,
                window_lo: 2,
                window_hi: 1,
            }
        };
        true
    }

    unsafe extern "C" fn rows(_instance: *mut c_void, out: *mut u32, cap: u32) -> u32 {
        let values = [10, 20, 30];
        let count = values.len().min(cap as usize);
        for (index, value) in values.into_iter().take(count).enumerate() {
            // SAFETY: the caller supplied `cap` writable entries.
            unsafe { *out.add(index) = value };
        }
        count as u32
    }

    fn plugin() -> RVPlaybackPlugin {
        // SAFETY: every field has a valid all-zero representation.
        let mut plugin: RVPlaybackPlugin = unsafe { core::mem::zeroed() };
        plugin.viz_info = Some(info);
        plugin.tracker_columns = Some(columns);
        plugin.tracker_channels = Some(channels);
        plugin.scope_channels = Some(channels);
        plugin.tracker_position = Some(position);
        plugin.tracker_cells = Some(cells);
        plugin.scope_samples = Some(scope);
        plugin.vu_levels = Some(vu);
        plugin
    }

    fn build_for_test(
        plugin: &RVPlaybackPlugin,
        output_frame: u64,
        scope_sample_budget: u32,
    ) -> Result<Option<VizSnapshot>, VisualizationError> {
        let config = VisualizationConfig {
            scope_sample_budget,
            ..VisualizationConfig::default()
        };
        let Some(layout) = prepare_layout(plugin, core::ptr::null_mut(), config)? else {
            return Ok(None);
        };
        let mut snapshot = layout.new_snapshot()?;
        capture_snapshot(
            plugin,
            core::ptr::null_mut(),
            &layout,
            output_frame,
            &mut snapshot,
        )?;
        Ok(Some(snapshot))
    }

    #[test]
    fn miri_unsafe_visualization_snapshot_transfers_between_threads() {
        let plugin = plugin();
        let snapshot = build_for_test(&plugin, 12_345, 64)
            .expect("valid snapshot")
            .expect("visualization");

        fn assert_send<T: Send>(_: &T) {}
        assert_send(&snapshot);
        let snapshot = std::thread::spawn(move || snapshot)
            .join()
            .expect("consumer thread");

        assert_eq!(snapshot.output_frame, 12_345);
        assert_eq!(snapshot.layout.caps, 7);
        assert_eq!(snapshot.layout.scroll_mode, RVScrollMode::Synchronized);
        assert_eq!(snapshot.layout.columns.len(), 3);
        assert_eq!(snapshot.layout.columns[2].kind, RVColumnKind::Effect as u32);
        assert_eq!(&snapshot.layout.pattern_channels[0].name[..4], b"left");
        assert_eq!(snapshot.layout.scope_channels.len(), 2);
        assert_eq!(snapshot.position.expect("position").row, 4);
        assert!(snapshot.channel_rows().is_empty());
        assert_eq!(snapshot.cells().len(), 8 * 2 * 3);
        assert_eq!(snapshot.cells()[47].text, [47; 16]);
        assert_eq!(snapshot.scope(0).expect("left scope").len(), 64);
        assert_eq!(snapshot.scope(0).expect("left scope")[0], 0.0);
        assert_eq!(snapshot.scope(0).expect("left scope")[63], 63.0 / 64.0);
        assert!(snapshot
            .scope(1)
            .expect("right scope")
            .iter()
            .all(|&sample| sample == 0.5));
        assert_eq!(snapshot.vu(), [0.8, 0.3]);
    }

    #[test]
    fn per_channel_rows_follow_scroll_mode() {
        let mut plugin = plugin();
        plugin.viz_info = Some(per_channel_info);
        plugin.tracker_channels = Some(three_channels);
        plugin.tracker_channel_rows = Some(rows);
        plugin.scope_samples = None;
        plugin.vu_levels = None;

        let snapshot = build_for_test(&plugin, 1, 64)
            .expect("valid snapshot")
            .expect("visualization");
        assert_eq!(snapshot.channel_rows(), [10, 20, 30]);
        assert!(snapshot.scope_counts().is_empty());
        assert!(snapshot.vu().is_empty());
    }

    #[test]
    fn absent_visualization_returns_none() {
        // SAFETY: every field has a valid all-zero representation.
        let plugin: RVPlaybackPlugin = unsafe { core::mem::zeroed() };
        assert!(build_for_test(&plugin, 0, 64)
            .expect("valid absence")
            .is_none());
    }

    #[test]
    fn scope_budget_and_plugin_counts_are_enforced() {
        let plugin = plugin();
        let snapshot = build_for_test(&plugin, 0, 7)
            .expect("valid snapshot")
            .expect("visualization");
        assert_eq!(snapshot.scope(0).expect("scope").len(), 7);
        assert_eq!(VisualizationConfig::default().scope_sample_budget, 2_048);

        let mut bad = plugin;
        bad.scope_samples = Some(overrun_scope);
        assert!(matches!(
            build_for_test(&bad, 0, 7),
            Err(VisualizationError::CountOverrun {
                query: "scope_samples",
                returned: 8,
                capacity: 7,
            })
        ));

        bad = plugin;
        bad.tracker_columns = Some(underfill_columns);
        assert!(matches!(
            build_for_test(&bad, 0, 7),
            Err(VisualizationError::CountUnderfill {
                query: "tracker_columns",
                returned: 2,
                expected: 3,
            })
        ));
    }

    #[test]
    fn invalid_structure_values_are_rejected() {
        let mut bad = plugin();
        bad.viz_info = Some(unknown_scroll_info);
        assert!(matches!(
            build_for_test(&bad, 0, 7),
            Err(VisualizationError::UnknownScrollMode(99))
        ));

        bad.viz_info = Some(overflowing_info);
        bad.tracker_columns = Some(full_columns);
        bad.tracker_channels = Some(full_channels);
        bad.scope_channels = None;
        bad.tracker_position = Some(one_row_position);
        assert!(matches!(
            build_for_test(&bad, 0, 7),
            Err(VisualizationError::DimensionLimit { .. })
        ));

        let mut bad = plugin();
        bad.tracker_position = Some(inverted_position);
        assert!(matches!(
            build_for_test(&bad, 0, 7),
            Err(VisualizationError::InvalidTrackerWindow {
                window_lo: 2,
                window_hi: 1,
            })
        ));
    }

    #[test]
    fn repeated_capture_reuses_layout_and_frame_storage() {
        INFO_CALLS.set(0);
        COLUMN_CALLS.set(0);
        CHANNEL_CALLS.set(0);

        let plugin = plugin();
        let config = VisualizationConfig {
            scope_sample_budget: 64,
            ..VisualizationConfig::default()
        };
        let layout = prepare_layout(&plugin, core::ptr::null_mut(), config)
            .expect("valid layout")
            .expect("visualization");
        let mut snapshot = layout.new_snapshot().expect("frame storage");
        let cell_ptr = snapshot.cells.as_ptr();
        let scope_ptr = snapshot.scope.as_ptr();
        let vu_ptr = snapshot.vu.as_ptr();

        crate::test_alloc::assert_no_alloc(|| {
            capture_snapshot(&plugin, core::ptr::null_mut(), &layout, 1, &mut snapshot)
                .expect("first capture");
            capture_snapshot(&plugin, core::ptr::null_mut(), &layout, 2, &mut snapshot)
                .expect("second capture");
        });

        assert_eq!(INFO_CALLS.get(), 1);
        assert_eq!(COLUMN_CALLS.get(), 1);
        assert_eq!(CHANNEL_CALLS.get(), 2);
        assert_eq!(snapshot.output_frame, 2);
        assert_eq!(snapshot.cells.as_ptr(), cell_ptr);
        assert_eq!(snapshot.scope.as_ptr(), scope_ptr);
        assert_eq!(snapshot.vu.as_ptr(), vu_ptr);
    }
}
