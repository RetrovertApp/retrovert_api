//! Owned visualization snapshots assembled beside synchronous decoding.

use core::ffi::c_void;

use crate::ffi::playback::{
    RVChannelDesc, RVColumnDesc, RVColumnKind, RVPatternCell, RVPlaybackPlugin, RVScrollMode,
    RVTrackerPosition, RVVizCaps, RVVizInfo,
};
use crate::session::Player;

pub const DEFAULT_SCOPE_SAMPLE_BUDGET: u32 = 2_048;

/// A coherent, frame-stamped copy of one plugin visualization update.
#[derive(Clone, Debug)]
pub struct VizSnapshot {
    pub output_frame: u64,
    pub caps: u32,
    pub scroll_mode: RVScrollMode,
    pub columns: Vec<RVColumnDesc>,
    pub pattern_channels: Vec<RVChannelDesc>,
    pub scope_channels: Vec<RVChannelDesc>,
    pub position: Option<RVTrackerPosition>,
    pub channel_rows: Vec<u32>,
    pub cells: Vec<RVPatternCell>,
    pub scope: Vec<Vec<f32>>,
    pub vu: Vec<f32>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VisualizationError {
    #[error("scroll mode {0} is unknown")]
    UnknownScrollMode(u32),
    #[error("{query} returned {returned} values for a buffer of {capacity}")]
    CountOverrun {
        query: &'static str,
        returned: u32,
        capacity: u32,
    },
    #[error("{query} returned {returned} values, expected {expected}")]
    CountUnderfill {
        query: &'static str,
        returned: u32,
        expected: u32,
    },
    #[error("visualization dimensions overflow the ABI count")]
    DimensionOverflow,
    #[error("could not allocate the {0} snapshot buffer")]
    Allocation(&'static str),
}

/// Controls snapshot assembly without owning cadence, buffering or threading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotBuilder {
    scope_sample_budget: u32,
}

impl Default for SnapshotBuilder {
    fn default() -> Self {
        Self {
            scope_sample_budget: DEFAULT_SCOPE_SAMPLE_BUDGET,
        }
    }
}

impl SnapshotBuilder {
    pub const fn new() -> Self {
        Self {
            scope_sample_budget: DEFAULT_SCOPE_SAMPLE_BUDGET,
        }
    }

    pub const fn scope_sample_budget(mut self, samples: u32) -> Self {
        self.scope_sample_budget = samples;
        self
    }

    pub const fn configured_scope_sample_budget(&self) -> u32 {
        self.scope_sample_budget
    }

    pub fn build(
        &self,
        player: &mut Player<'_>,
        output_frame: u64,
    ) -> Result<Option<VizSnapshot>, VisualizationError> {
        let (plugin, instance) = player.visualization_parts();
        build_snapshot(plugin, instance, output_frame, self.scope_sample_budget)
    }
}

fn buffer<T: Copy>(count: u32, zero: T, name: &'static str) -> Result<Vec<T>, VisualizationError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count as usize)
        .map_err(|_| VisualizationError::Allocation(name))?;
    values.resize(count as usize, zero);
    Ok(values)
}

fn read_vec<T: Copy>(
    count: u32,
    zero: T,
    name: &'static str,
    fill: impl FnOnce(*mut T, u32) -> u32,
) -> Result<Vec<T>, VisualizationError> {
    let mut values = buffer(count, zero, name)?;
    let returned = fill(values.as_mut_ptr(), count);
    if returned > count {
        return Err(VisualizationError::CountOverrun {
            query: name,
            returned,
            capacity: count,
        });
    }
    values.truncate(returned as usize);
    Ok(values)
}

fn read_exact<T: Copy>(
    count: u32,
    zero: T,
    name: &'static str,
    fill: impl FnOnce(*mut T, u32) -> u32,
) -> Result<Vec<T>, VisualizationError> {
    let values = read_vec(count, zero, name, fill)?;
    if values.len() != count as usize {
        return Err(VisualizationError::CountUnderfill {
            query: name,
            returned: values.len() as u32,
            expected: count,
        });
    }
    Ok(values)
}

fn cell_capacity(info: RVVizInfo, position: RVTrackerPosition) -> Result<u32, VisualizationError> {
    position
        .window_hi
        .saturating_sub(position.window_lo)
        .checked_mul(info.pattern_channel_count)
        .and_then(|count| count.checked_mul(info.column_count))
        .ok_or(VisualizationError::DimensionOverflow)
}

fn build_snapshot(
    plugin: &RVPlaybackPlugin,
    instance: *mut c_void,
    output_frame: u64,
    scope_sample_budget: u32,
) -> Result<Option<VizSnapshot>, VisualizationError> {
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

    let mut raw_position = RVTrackerPosition {
        order: 0,
        pattern: 0,
        row: 0,
        window_lo: 0,
        window_hi: 0,
    };
    let position = plugin.tracker_position.and_then(|callback| {
        // SAFETY: `raw_position` is writable and the instance is live.
        unsafe { callback(instance, &mut raw_position) }.then_some(raw_position)
    });

    let channel_rows = if scroll_mode == RVScrollMode::PerChannel {
        read_exact(
            info.pattern_channel_count,
            0,
            "tracker_channel_rows",
            |out, cap| {
                plugin.tracker_channel_rows.map_or(0, |callback| {
                    // SAFETY: `out` has `cap` writable entries and the instance is live.
                    unsafe { callback(instance, out, cap) }
                })
            },
        )?
    } else {
        Vec::new()
    };

    let cells = match position {
        Some(position) if info.caps & RVVizCaps::PATTERN_CELLS != 0 => {
            let count = cell_capacity(info, position)?;
            read_exact(
                count,
                RVPatternCell {
                    raw: 0,
                    text: [0; 16],
                },
                "tracker_cells",
                |out, cap| {
                    plugin.tracker_cells.map_or(0, |callback| {
                        // SAFETY: `out` has `cap` writable entries and the instance is live.
                        unsafe {
                            callback(
                                instance,
                                -1,
                                position.window_lo,
                                position.window_hi,
                                out,
                                cap,
                            )
                        }
                    })
                },
            )?
        }
        _ => Vec::new(),
    };

    let mut scope = Vec::new();
    if info.caps & RVVizCaps::SCOPE != 0 {
        scope
            .try_reserve_exact(info.scope_channel_count as usize)
            .map_err(|_| VisualizationError::Allocation("scope_channels"))?;
        if let Some(scope_samples) = plugin.scope_samples {
            for channel_index in 0..info.scope_channel_count {
                scope.push(read_vec(
                    scope_sample_budget,
                    0.0,
                    "scope_samples",
                    |out, cap| {
                        // SAFETY: `out` has `cap` writable entries and the instance is live.
                        unsafe { scope_samples(instance, channel_index as i32, out, cap) }
                    },
                )?);
            }
        }
    }

    let vu = if info.caps & RVVizCaps::VU != 0 {
        read_exact(info.scope_channel_count, 0.0, "vu_levels", |out, cap| {
            plugin.vu_levels.map_or(0, |callback| {
                // SAFETY: `out` has `cap` writable entries and the instance is live.
                unsafe { callback(instance, out, cap) }
            })
        })?
    } else {
        Vec::new()
    };

    Ok(Some(VizSnapshot {
        output_frame,
        caps: info.caps,
        scroll_mode,
        columns,
        pattern_channels,
        scope_channels,
        position,
        channel_rows,
        cells,
        scope,
        vu,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn info(_instance: *mut c_void, out: *mut RVVizInfo) -> bool {
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

    #[test]
    fn snapshot_matches_stub_across_threads() {
        let plugin = plugin();
        let snapshot = build_snapshot(&plugin, core::ptr::null_mut(), 12_345, 64)
            .expect("valid snapshot")
            .expect("visualization");

        fn assert_send_clone<T: Send + Clone>(_: &T) {}
        assert_send_clone(&snapshot);
        let clone = snapshot.clone();
        let output_frame = std::thread::spawn(move || clone.output_frame)
            .join()
            .expect("consumer thread");

        assert_eq!(output_frame, 12_345);
        assert_eq!(snapshot.caps, 7);
        assert_eq!(snapshot.scroll_mode, RVScrollMode::Synchronized);
        assert_eq!(snapshot.columns.len(), 3);
        assert_eq!(snapshot.columns[2].kind, RVColumnKind::Effect as u32);
        assert_eq!(&snapshot.pattern_channels[0].name[..4], b"left");
        assert_eq!(snapshot.scope_channels.len(), 2);
        assert_eq!(snapshot.position.expect("position").row, 4);
        assert!(snapshot.channel_rows.is_empty());
        assert_eq!(snapshot.cells.len(), 8 * 2 * 3);
        assert_eq!(snapshot.cells[47].text, [47; 16]);
        assert_eq!(snapshot.scope.len(), 2);
        assert_eq!(snapshot.scope[0].len(), 64);
        assert_eq!(snapshot.scope[0][0], 0.0);
        assert_eq!(snapshot.scope[0][63], 63.0 / 64.0);
        assert!(snapshot.scope[1].iter().all(|&sample| sample == 0.5));
        assert_eq!(snapshot.vu, [0.8, 0.3]);
    }

    #[test]
    fn per_channel_rows_follow_scroll_mode() {
        let mut plugin = plugin();
        plugin.viz_info = Some(per_channel_info);
        plugin.tracker_channels = Some(three_channels);
        plugin.tracker_channel_rows = Some(rows);
        plugin.scope_samples = None;
        plugin.vu_levels = None;

        let snapshot = build_snapshot(&plugin, core::ptr::null_mut(), 1, 64)
            .expect("valid snapshot")
            .expect("visualization");
        assert_eq!(snapshot.channel_rows, [10, 20, 30]);
        assert!(snapshot.scope.is_empty());
        assert!(snapshot.vu.is_empty());
    }

    #[test]
    fn absent_visualization_returns_none() {
        // SAFETY: every field has a valid all-zero representation.
        let plugin: RVPlaybackPlugin = unsafe { core::mem::zeroed() };
        assert!(build_snapshot(&plugin, core::ptr::null_mut(), 0, 64)
            .expect("valid absence")
            .is_none());
    }

    #[test]
    fn scope_budget_and_plugin_counts_are_enforced() {
        let plugin = plugin();
        let snapshot = build_snapshot(&plugin, core::ptr::null_mut(), 0, 7)
            .expect("valid snapshot")
            .expect("visualization");
        assert_eq!(snapshot.scope[0].len(), 7);
        assert_eq!(
            SnapshotBuilder::default().configured_scope_sample_budget(),
            2_048
        );

        let mut bad = plugin;
        bad.scope_samples = Some(overrun_scope);
        assert!(matches!(
            build_snapshot(&bad, core::ptr::null_mut(), 0, 7),
            Err(VisualizationError::CountOverrun {
                query: "scope_samples",
                returned: 8,
                capacity: 7,
            })
        ));

        bad = plugin;
        bad.tracker_columns = Some(underfill_columns);
        assert!(matches!(
            build_snapshot(&bad, core::ptr::null_mut(), 0, 7),
            Err(VisualizationError::CountUnderfill {
                query: "tracker_columns",
                returned: 2,
                expected: 3,
            })
        ));
    }

    #[test]
    fn invalid_structure_values_are_rejected() {
        let mut plugin = plugin();
        plugin.viz_info = Some(unknown_scroll_info);
        assert!(matches!(
            build_snapshot(&plugin, core::ptr::null_mut(), 0, 7),
            Err(VisualizationError::UnknownScrollMode(99))
        ));

        plugin.viz_info = Some(overflowing_info);
        plugin.tracker_columns = Some(full_columns);
        plugin.tracker_channels = Some(full_channels);
        plugin.scope_channels = None;
        plugin.tracker_position = Some(one_row_position);
        assert!(matches!(
            build_snapshot(&plugin, core::ptr::null_mut(), 0, 7),
            Err(VisualizationError::DimensionOverflow)
        ));
    }
}
