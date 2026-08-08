//! Walks the whole v2 ABI: layouts, enum discriminants and constants, then asserts
//! that nothing bindgen produced from the headers went unchecked.
//!
//! The field lists below follow header declaration order, so `#[rustfmt::skip]` keeps
//! them readable next to the headers instead of one identifier per line.

use core::mem::{align_of, offset_of, size_of};
use std::collections::HashSet;

use retrovert_host::ffi::audio_format::*;
use retrovert_host::ffi::io::*;
use retrovert_host::ffi::log::*;
use retrovert_host::ffi::metadata::*;
use retrovert_host::ffi::output::*;
use retrovert_host::ffi::playback::*;
use retrovert_host::ffi::resample::*;
use retrovert_host::ffi::service::*;
use retrovert_host::ffi::settings::*;

use crate::c;

const GENERATED: &str = include_str!(concat!(env!("OUT_DIR"), "/rv_abi.rs"));

/// Size of a struct field, without having to name its type. Offsets alone let a
/// narrowed field hide in the padding that follows it.
macro_rules! field_size {
    ($ty:ty, $field:ident) => {{
        fn size_of_pointee<T>(_: *const T) -> usize {
            size_of::<T>()
        }
        let value = core::mem::MaybeUninit::<$ty>::uninit();
        // SAFETY: forming a raw pointer to a field of uninitialised memory reads
        // nothing, and the pointer is only inspected for its pointee type.
        size_of_pointee(unsafe { &raw const (*value.as_ptr()).$field })
    }};
}

/// Size, alignment and every field's offset and width against the generated header type.
macro_rules! layout {
    ($covered:expr, $name:ident $(, $field:ident)* $(,)?) => {{
        $covered.mark(stringify!($name));
        assert_eq!(
            (size_of::<$name>(), align_of::<$name>()),
            (size_of::<c::$name>(), align_of::<c::$name>()),
            concat!(stringify!($name), ": size/align"),
        );
        $(assert_eq!(
            (offset_of!($name, $field), field_size!($name, $field)),
            (offset_of!(c::$name, $field), field_size!(c::$name, $field)),
            concat!(stringify!($name), ".", stringify!($field), ": offset/size"),
        );)*
    }};
}

/// Discriminants of a mirrored enum against the header's enumerators, in both
/// directions: the variant lowers to the C value, and `from_raw` maps it back.
macro_rules! values {
    ($covered:expr, $name:ident $(, $variant:ident = $c:ident)* $(,)?) => {{
        $covered.mark(stringify!($name));
        $(
            $covered.mark(stringify!($c));
            assert_eq!(i128::from($name::$variant as u32), i128::from(c::$c), stringify!($c));
            assert_eq!($name::from_raw(c::$c), Some($name::$variant), stringify!($c));
        )*
    }};
}

/// Bit values mirrored as constants against the header's enumerators.
macro_rules! bits {
    ($covered:expr, $name:ident $(, $konst:ident = $c:ident)* $(,)?) => {{
        $covered.mark(stringify!($name));
        $(
            $covered.mark(stringify!($c));
            assert_eq!($name::$konst, c::$c, stringify!($c));
        )*
    }};
}

/// A mirrored numeric `#define`.
macro_rules! scalar {
    ($covered:expr, $name:ident) => {{
        $covered.mark(stringify!($name));
        assert_eq!($name as i128, c::$name as i128, stringify!($name));
    }};
}

/// A mirrored string `#define`.
macro_rules! bytes {
    ($covered:expr, $name:ident) => {{
        $covered.mark(stringify!($name));
        assert_eq!($name.to_bytes_with_nul(), &c::$name[..], stringify!($name));
    }};
}

#[derive(Default)]
struct Covered(HashSet<&'static str>);

impl Covered {
    fn mark(&mut self, name: &'static str) {
        assert!(self.0.insert(name), "{name} checked twice");
    }

    fn assert_complete(&self) {
        let missing: Vec<&str> = GENERATED
            .lines()
            .filter_map(declared_name)
            .filter(|name| name.starts_with("RV") && !self.0.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "ABI items no parity check covers: {missing:?}"
        );
    }
}

fn declared_name(line: &str) -> Option<&str> {
    let tail = ["pub struct ", "pub union ", "pub type ", "pub const "]
        .iter()
        .find_map(|keyword| line.trim_start().strip_prefix(keyword))?;
    let end = tail.find(|ch: char| !ch.is_alphanumeric() && ch != '_')?;
    Some(&tail[..end])
}

#[test]
fn abi_parity() {
    let mut covered = Covered::default();

    audio_format(&mut covered);
    io(&mut covered);
    log(&mut covered);
    metadata(&mut covered);
    settings(&mut covered);
    service(&mut covered);
    playback(&mut covered);
    output(&mut covered);
    resample(&mut covered);
    opaque_handles(&mut covered);

    covered.assert_complete();
}

#[rustfmt::skip]
fn audio_format(covered: &mut Covered) {
    values!(covered, RVAudioStreamFormat,
        U8 = RVAudioStreamFormat_U8, S16 = RVAudioStreamFormat_S16,
        S24 = RVAudioStreamFormat_S24, S32 = RVAudioStreamFormat_S32,
        F32 = RVAudioStreamFormat_F32);
    layout!(covered, RVAudioFormat, audio_format, channel_count, sample_rate);
}

#[rustfmt::skip]
fn io(covered: &mut Covered) {
    scalar!(covered, RV_IO_API_VERSION);
    layout!(covered, RVIoReadUrlResult, data, data_size);
    layout!(covered, RVIo, private_data, exists, read_url_to_memory, free_url_to_memory);
}

#[rustfmt::skip]
fn log(covered: &mut Covered) {
    scalar!(covered, RV_LOG_API_VERSION);
    values!(covered, RVLogLevel,
        Trace = RVLogLevel_Trace, Debug = RVLogLevel_Debug, Info = RVLogLevel_Info,
        Warn = RVLogLevel_Warn, Error = RVLogLevel_Error, Fatal = RVLogLevel_Fatal);
    layout!(covered, RVLog, private_data, log);
}

#[rustfmt::skip]
fn metadata(covered: &mut Covered) {
    scalar!(covered, RV_METADATA_API_VERSION);
    bytes!(covered, RV_METADATA_TITLE_TAG);
    bytes!(covered, RV_METADATA_SONGTYPE_TAG);
    bytes!(covered, RV_METADATA_AUTHORINGTOOL_TAG);
    bytes!(covered, RV_METADATA_ARTIST_TAG);
    bytes!(covered, RV_METADATA_ALBUM_TAG);
    bytes!(covered, RV_METADATA_DATE_TAG);
    bytes!(covered, RV_METADATA_GENRE_TAG);
    bytes!(covered, RV_METADATA_MESSAGE_TAG);
    bytes!(covered, RV_METADATA_LENGTH_TAG);
    layout!(covered, RVMetadataId);
    values!(covered, RVMetaEncoding,
        Utf8 = RVMetaEncoding_Utf8, ShiftJS2 = RVMetaEncoding_ShiftJS2);
    values!(covered, RVRVMetadataResult,
        KeyNotFound = RVRVMetadataResult_KeyNotFound,
        UnableToMakeQuery = RVRVMetadataResult_UnableToMakeQuery);
    layout!(covered, RVMetadata, private_data, create_url, set_tag, set_tag_f64,
        add_subsong, add_sample, add_instrument);
}

#[rustfmt::skip]
fn settings(covered: &mut Covered) {
    scalar!(covered, RV_SETTINGS_API_VERSION);
    scalar!(covered, RVS_FLOAT_TYPE);
    scalar!(covered, RVS_INTEGER_TYPE);
    scalar!(covered, RVS_BOOL_TYPE);
    scalar!(covered, RVS_INTEGER_RANGE_TYPE);
    scalar!(covered, RVS_STRING_RANGE_TYPE);
    values!(covered, RVSettingsResult,
        Ok = RVSettingsResult_Ok, NotFound = RVSettingsResult_NotFound,
        UnknownId = RVSettingsResult_UnknownId, DuplicatedId = RVSettingsResult_DuplicatedId,
        WrongType = RVSettingsResult_WrongType);
    layout!(covered, RVSBase, widget_id, name, desc, widget_type);
    layout!(covered, RVSFloat, widget_id, name, desc, widget_type,
        value, start_range, end_range);
    layout!(covered, RVSInteger, widget_id, name, desc, widget_type,
        value, start_range, end_range);
    layout!(covered, RVSBool, widget_id, name, desc, widget_type, value);
    layout!(covered, RVSIntegerRangeValue, name, value);
    layout!(covered, RVSStringRangeValue, name, value);
    layout!(covered, RVSIntegerFixedRange, widget_id, name, desc, widget_type,
        value, values, values_size);
    layout!(covered, RVSStringFixedRange, widget_id, name, desc, widget_type,
        value, values, values_size);
    layout!(covered, RVSIntResult, result, value);
    layout!(covered, RVSFloatResult, result, value);
    layout!(covered, RVSStringResult, result, value);
    layout!(covered, RVSBoolResult, result, value);
    // Union members all start at offset 0, so size and alignment are the whole check;
    // the members themselves are checked above.
    layout!(covered, RVSetting);
    layout!(covered, RVSettings, private_data, reg, get_string, get_int, get_float, get_bool);
}

#[rustfmt::skip]
fn service(covered: &mut Covered) {
    layout!(covered, RVService, private_data, get_io, get_log, get_metadata, get_settings);
}

#[rustfmt::skip]
fn playback(covered: &mut Covered) {
    scalar!(covered, RV_PLAYBACK_PLUGIN_API_VERSION);
    values!(covered, RVProbeResult,
        Supported = RVProbeResult_Supported, Unsupported = RVProbeResult_Unsupported,
        Unsure = RVProbeResult_Unsure);
    values!(covered, RVReadStatus,
        DecodingRequest = RVReadStatus_DecodingRequest, Ok = RVReadStatus_Ok,
        Finished = RVReadStatus_Finished, Error = RVReadStatus_Error);
    values!(covered, RVSettingsUpdate,
        Default = RVSettingsUpdate_Default, RequireRestart = RVSettingsUpdate_RequireRestart);
    values!(covered, RVScrollMode,
        Synchronized = RVScrollMode_Synchronized, PerChannel = RVScrollMode_PerChannel);
    values!(covered, RVColumnKind,
        Note = RVColumnKind_Note, Instrument = RVColumnKind_Instrument,
        Volume = RVColumnKind_Volume, Effect = RVColumnKind_Effect,
        Param = RVColumnKind_Param, Custom = RVColumnKind_Custom);
    bits!(covered, RVVizCaps,
        PATTERN_CELLS = RVVizCaps_PatternCells, SCOPE = RVVizCaps_Scope, VU = RVVizCaps_Vu,
        WHOLE_SONG_KNOWN = RVVizCaps_WholeSongKnown,
        SEEKABLE_PREVIEW = RVVizCaps_SeekablePreview,
        FUTURE_KNOWN = RVVizCaps_FutureKnown);
    layout!(covered, RVReadInfo, format, frame_count, status);
    layout!(covered, RVReadData, channels_output, channels_output_max_bytes_size, info);
    layout!(covered, RVVizInfo, caps, scroll_mode, pattern_channel_count,
        scope_channel_count, column_count);
    layout!(covered, RVColumnDesc, label, char_width, kind);
    layout!(covered, RVChannelDesc, name, scope_width);
    layout!(covered, RVTrackerPosition, order, pattern, row, window_lo, window_hi);
    layout!(covered, RVPatternCell, raw, text);
    layout!(covered, RVPlaybackPlugin,
        api_version, name, version, library_version,
        probe_can_play, supported_extensions, create, destroy, event, open, close,
        read_data, seek, metadata, static_init, settings_updated, static_destroy,
        viz_info, tracker_columns, tracker_channels, scope_channels, tracker_position,
        tracker_channel_rows, tracker_cells, scope_enable, scope_samples, vu_levels);
}

#[rustfmt::skip]
fn output(covered: &mut Covered) {
    layout!(covered, RVWriteInfo, sample_rate, sample_count, channel_count, output_format);
    layout!(covered, RVPlaybackCallback, user_data, callback);
    layout!(covered, RVOutputTargets, names, names_size);
    layout!(covered, RVOutputPlugin,
        api_version, name, version, library_version,
        create, destroy, output_targets_info, start, stop, static_init);
}

#[rustfmt::skip]
fn resample(covered: &mut Covered) {
    values!(covered, RVResampleSettingsUpdate,
        Default = RVResampleSettingsUpdate_Default,
        RequireRestart = RVResampleSettingsUpdate_RequireRestart);
    layout!(covered, RVConvertConfig, input, output);
    layout!(covered, RVResamplePlugin,
        api_version, name, version, library_version,
        create, destroy, set_config, convert, get_expected_output_frame_count,
        get_required_input_frame_count, static_init, settings_updated);
}

/// Forward-declared handle types the mirror erases to `*mut c_void`.
fn opaque_handles(covered: &mut Covered) {
    for name in [
        "RVIoPrivate",
        "RVLogPrivate",
        "RVMetadataPrivate",
        "RVSettingsPrivate",
        "RVServicePrivData",
    ] {
        covered.mark(name);
    }
}

/// The completeness check is only as good as this parser: an item it stops recognising
/// goes uncovered silently, which reads exactly like a passing gate.
#[test]
fn declared_name_recognises_the_forms_bindgen_emits() {
    for (line, expected) in [
        ("pub struct RVAudioFormat {", Some("RVAudioFormat")),
        ("pub union RVSetting {", Some("RVSetting")),
        ("pub type RVMetadataId = u64;", Some("RVMetadataId")),
        (
            "pub const RV_IO_API_VERSION: u32 = 1;",
            Some("RV_IO_API_VERSION"),
        ),
        ("    pub const RVIndented: u32 = 1;", Some("RVIndented")),
        ("pub const RVWrappedAfterName:", Some("RVWrappedAfterName")),
        ("#[repr(C)]", None),
        ("#[derive(Copy, Clone)]", None),
        ("/// pub struct RVDocComment", None),
        ("struct RVNotPublic {", None),
        ("    pub audio_format: u32,", None),
        ("}", None),
    ] {
        assert_eq!(declared_name(line), expected, "{line}");
    }

    // The real generated file must still yield the item count the checks cover.
    let found = GENERATED.lines().filter_map(declared_name).count();
    assert!(
        found > 100,
        "parser found only {found} declarations in the bindings"
    );
}

/// `from_raw` rejects a discriminant the ABI does not define.
#[test]
fn unknown_discriminants_are_rejected() {
    assert_eq!(RVReadStatus::from_raw(1), Some(RVReadStatus::Ok));
    assert_eq!(RVReadStatus::from_raw(4), None);
    assert_eq!(RVAudioStreamFormat::from_raw(0), None);
}
