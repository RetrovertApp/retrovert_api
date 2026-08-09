use super::*;

use std::cell::RefCell;
use std::sync::Arc;

use crate::ffi::metadata::RVMetadata;
use crate::ffi::playback::{RVScrollMode, RVVizInfo};
use crate::service::{LogCrate, MemorySettingsStore, StdFsIo};
use crate::visualization::VisualizationConfig;

/// One scripted answer to `read_data`: the bytes to write, and the info to report. The
/// two are independent so a test can make the plugin lie about what it wrote.
#[derive(Clone)]
struct Response {
    bytes: Vec<u8>,
    sample_format: u32,
    channels: u32,
    sample_rate: u32,
    frames: u32,
    status: u32,
}

impl Response {
    /// Interleaved f32 frames, reported honestly.
    fn f32(samples: &[f32], channels: u32, sample_rate: u32) -> Self {
        Self {
            bytes: samples.iter().flat_map(|s| s.to_ne_bytes()).collect(),
            sample_format: RVAudioStreamFormat::F32 as u32,
            channels,
            sample_rate,
            frames: samples.len() as u32 / channels,
            status: RVReadStatus::Ok as u32,
        }
    }

    fn finished(mut self) -> Self {
        self.status = RVReadStatus::Finished as u32;
        self
    }
}

/// A plugin that never runs dry: every read gets the frames it asked for, carrying one
/// continuous mono ramp.
struct Ramp {
    next: f32,
    sample_rate: u32,
}

impl Ramp {
    fn take(&mut self) -> f32 {
        let value = self.next;
        self.next += 1.0;
        value
    }
}

/// What one `read_data` call was asked for.
#[derive(Clone, Copy)]
struct Call {
    max_bytes: u32,
    frames: u32,
    hint: RVAudioFormat,
    status: u32,
}

thread_local! {
    static RESPONSES: RefCell<Vec<Response>> = const { RefCell::new(Vec::new()) };
    static CALLS: RefCell<Vec<Call>> = const { RefCell::new(Vec::new()) };
    static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    static RAMP: RefCell<Option<Ramp>> = const { RefCell::new(None) };
    static OPEN_RESULT: RefCell<i32> = const { RefCell::new(0) };
    static SETTINGS_RESULT: RefCell<u32> = const { RefCell::new(0) };
    static LAST_FORMAT: RefCell<RVAudioFormat> = const {
        RefCell::new(RVAudioFormat {
            audio_format: RVAudioStreamFormat::F32 as u32,
            channel_count: 2,
            sample_rate: 48_000,
        })
    };
}

fn script(responses: Vec<Response>) {
    RESPONSES.with_borrow_mut(|slot| *slot = responses);
    RAMP.with_borrow_mut(|slot| *slot = None);
    CALLS.with_borrow_mut(Vec::clear);
    EVENTS.with_borrow_mut(Vec::clear);
    OPEN_RESULT.with_borrow_mut(|slot| *slot = 0);
}

fn script_ramp(sample_rate: u32) {
    script(Vec::new());
    RAMP.with_borrow_mut(|slot| {
        *slot = Some(Ramp {
            next: 0.0,
            sample_rate,
        })
    });
}

fn calls() -> Vec<Call> {
    CALLS.with_borrow(Clone::clone)
}

fn events() -> Vec<&'static str> {
    EVENTS.with_borrow(Clone::clone)
}

fn note(event: &'static str) {
    EVENTS.with_borrow_mut(|events| events.push(event));
}

extern "C" fn stub_static_init(_services: *const RVService) {
    note("static_init");
}

extern "C" fn stub_static_destroy() {
    note("static_destroy");
}

extern "C" fn stub_create(_services: *const RVService) -> *mut c_void {
    note("create");
    Box::into_raw(Box::new(0_u8)).cast()
}

unsafe extern "C" fn stub_destroy(user_data: *mut c_void) -> i32 {
    note("destroy");
    // SAFETY: `create` handed out exactly this allocation, and it is freed once.
    drop(unsafe { Box::from_raw(user_data.cast::<u8>()) });
    0
}

extern "C" fn stub_open(
    _user_data: *mut c_void,
    _url: *const core::ffi::c_char,
    _subsong: u32,
    _services: *const RVService,
) -> i32 {
    note("open");
    OPEN_RESULT.with_borrow(|result| *result)
}

extern "C" fn stub_close(_user_data: *mut c_void) {
    note("close");
}

extern "C" fn stub_settings_updated(_user_data: *mut c_void, _services: *const RVService) -> u32 {
    SETTINGS_RESULT.with_borrow(|result| *result)
}

unsafe extern "C" fn stub_viz_info(_user_data: *mut c_void, out: *mut RVVizInfo) -> bool {
    note("viz_info");
    // SAFETY: the snapshot builder supplies a writable `RVVizInfo`.
    unsafe {
        *out = RVVizInfo {
            caps: 0,
            scroll_mode: RVScrollMode::Synchronized as u32,
            pattern_channel_count: 0,
            scope_channel_count: 0,
            column_count: 0,
        }
    };
    true
}

extern "C" fn stub_scope_enable(_user_data: *mut c_void, enabled: bool) {
    note(if enabled { "scope_on" } else { "scope_off" });
}

unsafe extern "C" fn stub_read_data(_user_data: *mut c_void, dest: RVReadData) -> RVReadInfo {
    CALLS.with_borrow_mut(|calls| {
        calls.push(Call {
            max_bytes: dest.channels_output_max_bytes_size,
            frames: dest.info.frame_count,
            hint: dest.info.format,
            status: dest.info.status,
        })
    });

    // A ramp plugin answers every request in full, so the session's own accounting across
    // repeated reads is what the test is left looking at.
    let ramp = RAMP.with_borrow_mut(|ramp| {
        ramp.as_mut().map(|ramp| {
            let frames = dest.info.frame_count;
            let samples: Vec<f32> = (0..frames).map(|_| ramp.take()).collect();
            Response::f32(&samples, 1, ramp.sample_rate)
        })
    });

    let response = ramp.or_else(|| {
        RESPONSES.with_borrow_mut(|responses| {
            if responses.is_empty() {
                None
            } else {
                Some(responses.remove(0))
            }
        })
    });
    // Out of script: end the song in whatever format the last answer set, so exhaustion
    // never looks like a format change.
    let Some(response) = response else {
        return RVReadInfo {
            format: LAST_FORMAT.with_borrow(|format| *format),
            frame_count: 0,
            status: RVReadStatus::Finished as u32,
        };
    };
    LAST_FORMAT.with_borrow_mut(|format| {
        *format = RVAudioFormat {
            audio_format: response.sample_format,
            channel_count: response.channels,
            sample_rate: response.sample_rate,
        }
    });

    let count = response
        .bytes
        .len()
        .min(dest.channels_output_max_bytes_size as usize);
    // SAFETY: the host advertised `channels_output_max_bytes_size` writable bytes at
    // `channels_output`, and no more than that many are written.
    unsafe {
        core::ptr::copy_nonoverlapping(
            response.bytes.as_ptr(),
            dest.channels_output.cast::<u8>(),
            count,
        )
    };

    RVReadInfo {
        format: RVAudioFormat {
            audio_format: response.sample_format,
            channel_count: response.channels,
            sample_rate: response.sample_rate,
        },
        frame_count: response.frames,
        status: response.status,
    }
}

/// A steady-state fixture which performs no bookkeeping or temporary allocation of its own.
unsafe extern "C" fn allocation_free_read_data(
    _user_data: *mut c_void,
    dest: RVReadData,
) -> RVReadInfo {
    let format = LAST_FORMAT.with_borrow(|format| *format);
    let width = RVAudioStreamFormat::from_raw(format.audio_format)
        .map(convert::sample_width)
        .unwrap_or(core::mem::size_of::<f32>());
    let bytes = dest.info.frame_count as usize * format.channel_count as usize * width;
    // SAFETY: the fixture uses only valid mono/stereo ABI formats, all no wider than the
    // widest stereo frame the host advertises for every requested frame.
    unsafe { core::ptr::write_bytes(dest.channels_output.cast::<u8>(), 0, bytes) };
    RVReadInfo {
        format,
        frame_count: dest.info.frame_count,
        status: RVReadStatus::Ok as u32,
    }
}

fn descriptor() -> RVPlaybackPlugin {
    // SAFETY: every field is a nullable pointer, an `Option<fn>` or an integer, and the
    // zero pattern is the null / `None` / zero case for all of them.
    let mut plugin: RVPlaybackPlugin = unsafe { core::mem::zeroed() };
    plugin.api_version = 2;
    plugin.static_init = Some(stub_static_init);
    plugin.static_destroy = Some(stub_static_destroy);
    plugin.create = Some(stub_create);
    plugin.destroy = Some(stub_destroy);
    plugin.open = Some(stub_open);
    plugin.close = Some(stub_close);
    plugin.read_data = Some(stub_read_data);
    plugin.settings_updated = Some(stub_settings_updated);
    plugin.viz_info = Some(stub_viz_info);
    plugin.scope_enable = Some(stub_scope_enable);
    plugin
}

fn host() -> ServiceHost {
    ServiceHost::new(
        Box::new(StdFsIo),
        Arc::new(LogCrate),
        Box::new(MemorySettingsStore::default()),
    )
}

/// Runs `body` against a plugin over `descriptor`, leaving the script alone.
fn with_descriptor<R>(descriptor: RVPlaybackPlugin, body: impl FnOnce(&Plugin<'_>) -> R) -> R {
    let host = host();
    let plugin = Plugin::from_descriptor(&descriptor, &host);
    body(&plugin)
}

fn with_plugin<R>(body: impl FnOnce(&Plugin<'_>) -> R) -> R {
    with_descriptor(descriptor(), body)
}

/// Runs `body` against a player opened over the scripted responses.
fn with_player<R>(
    responses: Vec<Response>,
    target: Option<StreamFormat>,
    body: impl FnOnce(&mut PreparedPlayer<'_>) -> R,
) -> R {
    script(responses);
    with_plugin(|plugin| {
        let player = match target {
            Some(target) => plugin
                .open_with_target("song.mod", 0, target)
                .expect("open"),
            None => plugin.open("song.mod", 0).expect("open"),
        };
        let mut player = player.prepare(1_024).expect("prepare");
        body(&mut player)
    })
}

/// The samples one read produced, with the format they came in.
fn read(
    responses: Vec<Response>,
    target: Option<StreamFormat>,
    frames: u32,
) -> (Vec<f32>, StreamFormat, bool) {
    with_player(responses, target, |player| {
        let chunk = player.read(frames).expect("read");
        (chunk.samples.to_vec(), chunk.format, chunk.finished)
    })
}

fn read_error(responses: Vec<Response>, target: Option<StreamFormat>) -> SessionError {
    with_player(responses, target, |player| {
        player.read(4).expect_err("read should fail")
    })
}

/// A response carrying `frames` frames of raw `bytes` in `format`.
fn raw(bytes: Vec<u8>, format: RVAudioStreamFormat, frames: u32, channels: u32) -> Response {
    Response {
        bytes,
        sample_format: format as u32,
        channels,
        sample_rate: 48_000,
        frames,
        status: RVReadStatus::Ok as u32,
    }
}

#[test]
fn a_native_chunk_describes_itself() {
    let (samples, format, finished) = read(vec![Response::f32(&[0.25, -0.25], 1, 22_050)], None, 2);
    assert_eq!(samples, [0.25, -0.25]);
    assert_eq!(
        format,
        StreamFormat {
            sample_rate: 22_050,
            channels: 1
        }
    );
    assert!(!finished);
}

#[test]
fn repeated_reads_at_the_prepared_budget_do_not_allocate() {
    let cases = [
        (
            RVAudioFormat {
                audio_format: RVAudioStreamFormat::F32 as u32,
                channel_count: 1,
                sample_rate: 48_000,
            },
            None,
        ),
        (
            RVAudioFormat {
                audio_format: RVAudioStreamFormat::S16 as u32,
                channel_count: 2,
                sample_rate: 48_000,
            },
            None,
        ),
        (
            RVAudioFormat {
                audio_format: RVAudioStreamFormat::F32 as u32,
                channel_count: 1,
                sample_rate: 48_000,
            },
            Some(StreamFormat {
                sample_rate: 48_000,
                channels: 2,
            }),
        ),
        (
            RVAudioFormat {
                audio_format: RVAudioStreamFormat::F32 as u32,
                channel_count: 1,
                sample_rate: 24_000,
            },
            Some(StreamFormat {
                sample_rate: 48_000,
                channels: 1,
            }),
        ),
        (
            RVAudioFormat {
                audio_format: RVAudioStreamFormat::S16 as u32,
                channel_count: 1,
                sample_rate: 24_000,
            },
            Some(StreamFormat {
                sample_rate: 48_000,
                channels: 2,
            }),
        ),
    ];

    for (native, target) in cases {
        script(Vec::new());
        LAST_FORMAT.with_borrow_mut(|format| *format = native);
        let mut descriptor = descriptor();
        descriptor.read_data = Some(allocation_free_read_data);

        with_descriptor(descriptor, |plugin| {
            let player = match target {
                Some(target) => plugin
                    .open_with_target("song.mod", 0, target)
                    .expect("open"),
                None => plugin.open("song.mod", 0).expect("open"),
            };
            let mut player = player.prepare(256).expect("prepare");

            crate::test_alloc::assert_no_alloc(|| {
                for _ in 0..2 {
                    let chunk = player.read(256).expect("prepared read");
                    assert_eq!(chunk.frames(), 256);
                }
            });
        });
    }
}

#[test]
fn every_stream_format_normalizes_to_f32() {
    let cases = [
        (RVAudioStreamFormat::U8, vec![255_u8, 128], 1.0_f32),
        (RVAudioStreamFormat::S16, vec![0, 0x40, 0, 0], 0.5),
        (RVAudioStreamFormat::S24, vec![0, 0, 0x40, 0, 0, 0], 0.5),
        (
            RVAudioStreamFormat::S32,
            vec![0, 0, 0, 0x40, 0, 0, 0, 0],
            0.5,
        ),
        (
            RVAudioStreamFormat::F32,
            0.5_f32
                .to_ne_bytes()
                .into_iter()
                .chain(0.0_f32.to_ne_bytes())
                .collect(),
            0.5,
        ),
    ];
    for (format, bytes, expected) in cases {
        let (samples, ..) = read(vec![raw(bytes, format, 2, 1)], None, 4);
        assert_eq!(samples.len(), 2, "{format:?}");
        assert!(
            (samples[0] - expected).abs() < 0.01,
            "{format:?} gave {samples:?}"
        );
        assert_eq!(samples[1], 0.0, "{format:?}");
    }
}

#[test]
fn the_first_chunk_locks_every_leg_of_the_format() {
    // Rate, channel count and sample format each lock on their own.
    let mut rate = Response::f32(&[0.0, 0.0], 2, 44_100);
    rate.sample_rate = 48_000;

    let mut channels = Response::f32(&[0.0, 0.0], 2, 44_100);
    channels.channels = 1;
    channels.frames = 2;

    let mut sample_format = Response::f32(&[0.0, 0.0], 2, 44_100);
    sample_format.sample_format = RVAudioStreamFormat::S16 as u32;

    for second in [rate, channels, sample_format] {
        let error = read_error(vec![Response::f32(&[0.0, 0.0], 2, 44_100), second], None);
        assert!(
            matches!(error, SessionError::Abi(AbiViolation::FormatChanged { .. })),
            "{error}"
        );
    }
}

#[test]
fn a_locked_session_reports_the_native_format() {
    with_player(
        vec![Response::f32(&[0.0, 0.0], 2, 44_100)],
        None,
        |player| {
            assert_eq!(player.native_format(), None);
            player.read(4).expect("read");
            assert_eq!(
                player.native_format(),
                Some(FormatLock {
                    sample_format: RVAudioStreamFormat::F32,
                    sample_rate: 44_100,
                    channels: 2,
                })
            );
        },
    );
}

#[test]
fn a_zero_sample_rate_is_rejected() {
    let mut response = Response::f32(&[0.0], 1, 0);
    response.sample_rate = 0;
    assert_eq!(
        read_error(vec![response], None),
        SessionError::Abi(AbiViolation::ZeroSampleRate)
    );
}

#[test]
fn an_absurd_native_rate_is_rejected_before_scaled_allocation() {
    let native = 4_000_000_000;
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    assert_eq!(
        read_error(vec![Response::f32(&[], 2, native)], Some(target)),
        SessionError::Abi(AbiViolation::SampleRateRatio {
            native,
            target: target.sample_rate,
            max_ratio: MAX_SAMPLE_RATE_RATIO,
        })
    );
    assert_eq!(calls().len(), 1);
    assert_eq!(calls()[0].frames, 4);
}

#[test]
fn ordinary_sample_rate_extremes_are_accepted() {
    for (native, target_rate) in [(384_000, 8_000), (8_000, 384_000)] {
        let target = StreamFormat {
            sample_rate: target_rate,
            channels: 1,
        };
        with_player(
            vec![Response::f32(&[0.0], 1, native)],
            Some(target),
            |player| {
                player.read(4).expect("ordinary rate");
            },
        );
    }
}

#[test]
fn channel_counts_outside_mono_and_stereo_are_rejected() {
    for channels in [0, 3, 8] {
        let mut response = Response::f32(&[0.0, 0.0, 0.0, 0.0], 1, 48_000);
        response.channels = channels;
        response.frames = 1;
        assert_eq!(
            read_error(vec![response], None),
            SessionError::Abi(AbiViolation::ChannelCount(channels))
        );
    }
}

#[test]
fn a_frame_count_past_the_request_is_rejected() {
    let mut response = Response::f32(&[0.0, 0.0], 1, 48_000);
    response.frames = 5;
    assert_eq!(
        read_error(vec![response], None),
        SessionError::Abi(AbiViolation::FrameCount {
            returned: 5,
            requested: 4
        })
    );
}

#[test]
fn audio_that_would_not_fit_the_buffer_is_rejected() {
    // Stereo S32 needs eight bytes a frame, so five frames overrun the 32 bytes the
    // session advertised for the four it asked for.
    let mut response = raw(vec![0; 40], RVAudioStreamFormat::S32, 5, 2);
    response.sample_rate = 48_000;
    assert_eq!(
        read_error(vec![response], None),
        SessionError::Abi(AbiViolation::BufferOverrun {
            needed: 40,
            available: 32
        })
    );
}

#[test]
fn unknown_and_request_statuses_are_rejected() {
    let mut unknown = Response::f32(&[0.0], 1, 48_000);
    unknown.status = 77;
    assert_eq!(
        read_error(vec![unknown], None),
        SessionError::Abi(AbiViolation::UnknownStatus(77))
    );

    let mut request = Response::f32(&[0.0], 1, 48_000);
    request.status = RVReadStatus::DecodingRequest as u32;
    assert_eq!(
        read_error(vec![request], None),
        SessionError::Abi(AbiViolation::RequestStatus)
    );
}

#[test]
fn an_unknown_sample_format_is_rejected() {
    let mut response = Response::f32(&[0.0], 1, 48_000);
    response.sample_format = 99;
    assert_eq!(
        read_error(vec![response], None),
        SessionError::Abi(AbiViolation::UnknownSampleFormat(99))
    );
}

#[test]
fn an_error_status_ends_the_session() {
    let mut response = Response::f32(&[0.0], 1, 48_000);
    response.status = RVReadStatus::Error as u32;
    assert_eq!(read_error(vec![response], None), SessionError::Decode);
}

#[test]
fn a_finished_chunk_stops_later_reads() {
    with_player(
        vec![Response::f32(&[0.5, 0.5], 1, 48_000).finished()],
        None,
        |player| {
            let chunk = player.read(4).expect("read");
            assert_eq!(chunk.frames(), 2);
            assert!(chunk.finished);

            let chunk = player.read(4).expect("read past the end");
            assert!(chunk.samples.is_empty());
            assert!(chunk.finished);
            assert_eq!(calls().len(), 1, "the plugin was read after it finished");
        },
    );
}

#[test]
fn a_dry_read_stops_the_budget_without_finishing() {
    with_player(vec![Response::f32(&[], 1, 48_000)], None, |player| {
        let chunk = player.read(4).expect("read");
        assert!(chunk.samples.is_empty());
        assert!(!chunk.finished);
        assert_eq!(calls().len(), 1);
    });
}

#[test]
fn short_chunks_are_topped_up_until_the_budget_is_full() {
    let (samples, format, _) = read(
        vec![
            Response::f32(&[0.1, 0.2], 1, 48_000),
            Response::f32(&[0.3, 0.4], 1, 48_000),
        ],
        None,
        4,
    );
    assert_eq!(samples, [0.1, 0.2, 0.3, 0.4]);
    assert_eq!(format.channels, 1);
    assert_eq!(calls().len(), 2);

    let second = calls()[1];
    assert_eq!(second.frames, 2, "the second read asks for the remainder");
    // The hint rides every read, not just the first.
    assert_eq!(second.hint.audio_format, RVAudioStreamFormat::F32 as u32);
    assert_eq!(second.hint.channel_count, DEFAULT_HINT.channels);
    assert_eq!(second.hint.sample_rate, DEFAULT_HINT.sample_rate);
    assert_eq!(second.status, RVReadStatus::DecodingRequest as u32);
}

#[test]
fn the_read_hint_carries_the_default_when_no_target_is_set() {
    read(vec![Response::f32(&[0.0], 1, 48_000)], None, 4);
    let call = calls()[0];
    assert_eq!(call.hint.audio_format, RVAudioStreamFormat::F32 as u32);
    assert_eq!(call.hint.channel_count, DEFAULT_HINT.channels);
    assert_eq!(call.hint.sample_rate, DEFAULT_HINT.sample_rate);
    assert_eq!(call.status, RVReadStatus::DecodingRequest as u32);
    assert_eq!(call.frames, 4);
    assert_eq!(call.max_bytes, 4 * 2 * 4);
}

#[test]
fn the_read_hint_carries_the_target_when_one_is_set() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    read(vec![Response::f32(&[0.0, 0.0], 2, 48_000)], Some(target), 4);
    let call = calls()[0];
    assert_eq!(call.hint.channel_count, 2);
    assert_eq!(call.hint.sample_rate, 48_000);
}

#[test]
fn a_target_at_the_native_rate_only_adapts_channels() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 2,
    };
    let (samples, format, _) = read(
        vec![Response::f32(&[0.5, -0.5], 1, 48_000)],
        Some(target),
        2,
    );
    assert_eq!(samples, [0.5, 0.5, -0.5, -0.5]);
    assert_eq!(format, target);
}

#[test]
fn stereo_folds_down_to_a_mono_target() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    let (samples, ..) = read(
        vec![Response::f32(&[1.0, 0.0, 0.5, 0.5], 2, 48_000)],
        Some(target),
        2,
    );
    assert_eq!(samples, [0.5, 0.5]);
}

#[test]
fn a_target_rate_resamples() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    let (samples, format, _) = read(
        vec![Response::f32(&[0.0, 1.0, 2.0, 3.0], 1, 24_000)],
        Some(target),
        4,
    );
    assert_eq!(samples, [0.0, 0.5, 1.0, 1.5]);
    assert_eq!(format, target);
}

#[test]
fn resampling_asks_for_scaled_source_frames() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    let (samples, ..) = read(
        vec![
            Response::f32(&[0.0, 1.0, 2.0, 3.0], 1, 24_000),
            Response::f32(&[10.0, 11.0], 1, 24_000),
        ],
        Some(target),
        8,
    );
    // The first read cannot know the rate yet, so it asks for the whole budget; the
    // second wants two more output frames, which is one source frame plus slack.
    assert_eq!(calls()[0].frames, 8);
    assert_eq!(calls()[1].frames, 1 + 2);
    assert_eq!(samples, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 6.5]);
}

#[test]
fn an_unsupported_channel_adaptation_is_refused() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 4,
    };
    assert_eq!(
        read_error(vec![Response::f32(&[0.0, 0.0], 2, 48_000)], Some(target)),
        SessionError::ChannelAdaptation { from: 2, to: 4 }
    );
}

#[test]
fn an_unplayable_target_is_refused_at_open() {
    script(Vec::new());
    let targets = [
        StreamFormat {
            sample_rate: 0,
            channels: 2,
        },
        StreamFormat {
            sample_rate: 48_000,
            channels: 0,
        },
        StreamFormat {
            sample_rate: 48_000,
            channels: MAX_TARGET_CHANNELS + 1,
        },
    ];
    with_plugin(|plugin| {
        for target in targets {
            assert_eq!(
                plugin.open_with_target("song.mod", 0, target).err(),
                Some(SessionError::InvalidTarget(target))
            );
        }
    });
    assert!(
        !events().contains(&"create"),
        "an instance was created anyway"
    );
}

#[test]
fn a_url_with_an_interior_nul_is_refused() {
    script(Vec::new());
    with_plugin(|plugin| {
        assert_eq!(
            plugin.open("song\0.mod", 0).err(),
            Some(SessionError::InvalidUrl)
        );
    });
    assert!(
        !events().contains(&"create"),
        "an instance was created anyway"
    );
}

#[test]
fn the_session_brackets_the_plugin_in_order() {
    script(vec![Response::f32(&[0.0], 1, 48_000)]);
    with_plugin(|plugin| {
        let mut player = plugin
            .open("song.mod", 0)
            .expect("open")
            .prepare(1)
            .expect("prepare");
        player.read(1).expect("read");
    });
    assert_eq!(
        events(),
        [
            "static_init",
            "create",
            "open",
            "close",
            "destroy",
            "static_destroy"
        ]
    );
}

#[test]
fn the_player_forwards_scope_control_and_builds_snapshots() {
    with_player(Vec::new(), None, |player| {
        player.set_scope_enabled(true);
        let layout = player
            .prepare_visualization(VisualizationConfig::default())
            .expect("valid visualization")
            .expect("visualization surface");
        let cached = player
            .prepare_visualization(VisualizationConfig::default())
            .expect("cached visualization")
            .expect("visualization surface");
        assert!(Arc::ptr_eq(&layout, &cached));
        let mut snapshot = layout.new_snapshot().expect("snapshot storage");
        player
            .capture_visualization(321, &mut snapshot)
            .expect("capture");
        player.set_scope_enabled(false);

        assert_eq!(snapshot.output_frame, 321);
        assert_eq!(snapshot.layout.caps, 0);
        assert_eq!(snapshot.layout.scroll_mode, RVScrollMode::Synchronized);
    });
    let events = events();
    assert_eq!(
        events.iter().filter(|&&event| event == "viz_info").count(),
        1
    );
    assert!(events
        .windows(3)
        .any(|events| { events == ["scope_on", "viz_info", "scope_off"] }));
}

#[test]
fn a_failed_open_destroys_without_closing() {
    script(Vec::new());
    OPEN_RESULT.with_borrow_mut(|result| *result = -1);
    with_plugin(|plugin| {
        assert_eq!(
            plugin.open("song.mod", 0).err(),
            Some(SessionError::OpenFailed(-1))
        );
    });
    assert_eq!(
        events(),
        ["static_init", "create", "open", "destroy", "static_destroy"]
    );
}

#[test]
fn a_failed_open_discards_what_it_pushed() {
    script(Vec::new());
    OPEN_RESULT.with_borrow_mut(|result| *result = -1);
    let host = host();
    let descriptor = descriptor();
    let metadata = host.metadata();

    // SAFETY: the vtable comes from the live host, and the strings are NUL-terminated.
    unsafe {
        let services = host.service();
        let vtable = &*(services.get_metadata.expect("get_metadata"))(
            services.private_data,
            crate::ffi::metadata::RV_METADATA_API_VERSION,
        );
        push_title(vtable, services.private_data);
    }

    let plugin = Plugin::from_descriptor(&descriptor, &host);
    assert!(plugin.open("song.mod", 0).is_err());
    assert!(
        metadata.take().tags.is_empty(),
        "the failed open left its pushes behind"
    );
}

/// # Safety
///
/// `vtable` and `private_data` must belong to a live host.
unsafe fn push_title(vtable: &RVMetadata, private_data: *mut c_void) {
    let key = c"title";
    let value = c"half a song";
    // SAFETY: the caller guarantees the host, and both strings outlive the call.
    unsafe { (vtable.set_tag.expect("set_tag"))(private_data, 0, key.as_ptr(), value.as_ptr()) };
}

#[test]
fn settings_updated_reports_what_the_plugin_answered() {
    SETTINGS_RESULT.with_borrow_mut(|result| *result = RVSettingsUpdate::RequireRestart as u32);
    with_player(Vec::new(), None, |player| {
        assert_eq!(player.settings_updated(), RVSettingsUpdate::RequireRestart);
    });

    // Anything else, including a discriminant this ABI does not define, is live.
    SETTINGS_RESULT.with_borrow_mut(|result| *result = 42);
    with_player(Vec::new(), None, |player| {
        assert_eq!(player.settings_updated(), RVSettingsUpdate::Default);
    });
    SETTINGS_RESULT.with_borrow_mut(|result| *result = 0);
}

#[test]
fn a_plugin_without_settings_updated_reports_a_live_change() {
    script(Vec::new());
    let mut descriptor = descriptor();
    descriptor.settings_updated = None;
    with_descriptor(descriptor, |plugin| {
        let mut player = plugin.open("song.mod", 0).expect("open");
        assert_eq!(player.settings_updated(), RVSettingsUpdate::Default);
    });
}

#[test]
fn a_plugin_missing_a_playback_callback_cannot_open() {
    script(Vec::new());
    for name in ["create", "open", "read_data"] {
        let mut descriptor = descriptor();
        match name {
            "create" => descriptor.create = None,
            "open" => descriptor.open = None,
            _ => descriptor.read_data = None,
        }
        with_descriptor(descriptor, |plugin| {
            assert_eq!(
                plugin.open("song.mod", 0).err(),
                Some(SessionError::MissingCallback(name))
            );
        });
    }
}

#[test]
fn a_null_instance_is_refused() {
    extern "C" fn no_instance(_services: *const RVService) -> *mut c_void {
        core::ptr::null_mut()
    }

    script(Vec::new());
    let mut descriptor = descriptor();
    descriptor.create = Some(no_instance);
    with_descriptor(descriptor, |plugin| {
        assert_eq!(
            plugin.open("song.mod", 0).err(),
            Some(SessionError::CreateFailed)
        );
    });
}

#[test]
fn a_zero_budget_reads_nothing() {
    with_player(vec![Response::f32(&[0.5], 1, 48_000)], None, |player| {
        // Nothing is known about the stream until the plugin has answered once.
        assert_eq!(
            player.format(),
            StreamFormat {
                sample_rate: 0,
                channels: 0
            }
        );
        let chunk = player.read(0).expect("read");
        assert!(chunk.samples.is_empty());
        assert_eq!(chunk.frames(), 0);
        assert!(calls().is_empty());
    });
}

#[test]
fn repeated_resampled_reads_stay_in_step() {
    script_ramp(44_100);
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    let step = 44_100.0 / 48_000.0;
    with_plugin(|plugin| {
        let mut player = plugin
            .open_with_target("song.mod", 0, target)
            .expect("open")
            .prepare(480)
            .expect("prepare");
        let mut stream = Vec::new();
        for read in 0..4 {
            let chunk = player.read(480).expect("read");
            assert_eq!(chunk.frames(), 480, "read {read} came up short");
            stream.extend_from_slice(chunk.samples);
        }
        // One continuous ramp across the joins as well as within each chunk: a frame
        // dropped at a read boundary shows up here as a jump.
        for (index, pair) in stream.windows(2).enumerate() {
            let advance = pair[1] - pair[0];
            assert!(
                (advance - step).abs() < 0.01,
                "frame {index} advanced by {advance} at {pair:?}"
            );
        }
    });
}

#[test]
fn a_carry_larger_than_the_budget_serves_several_reads() {
    script_ramp(8_000);
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    let step = 8_000.0 / 48_000.0;
    with_plugin(|plugin| {
        let mut player = plugin
            .open_with_target("song.mod", 0, target)
            .expect("open")
            .prepare(4)
            .expect("prepare");
        let mut stream = Vec::new();
        for read in 0..4 {
            let chunk = player.read(4).expect("read");
            assert_eq!(chunk.frames(), 4, "read {read} came up short");
            stream.extend_from_slice(chunk.samples);
        }
        // Four source frames sixfold into eighteen, so one pull covers every read and
        // the carry is served — and moved — three times over.
        assert_eq!(calls().len(), 1, "the plugin was read again");
        for (index, pair) in stream.windows(2).enumerate() {
            let advance = pair[1] - pair[0];
            assert!(
                (advance - step).abs() < 0.001,
                "frame {index} advanced by {advance} at {pair:?}"
            );
        }
    });
}

#[test]
fn a_finished_song_still_delivers_its_surplus() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    with_player(
        vec![Response::f32(&[0.0, 1.0, 2.0, 3.0], 1, 24_000).finished()],
        Some(target),
        |player| {
            // Four source frames double to six, two of them past this budget.
            let chunk = player.read(4).expect("read");
            assert_eq!(chunk.samples, [0.0, 0.5, 1.0, 1.5]);
            assert!(!chunk.finished, "audio is still owed");

            let chunk = player.read(4).expect("read");
            assert_eq!(chunk.samples, [2.0, 2.5]);
            assert!(chunk.finished);
            assert_eq!(
                calls().len(),
                1,
                "the plugin was read again for the surplus"
            );
        },
    );
}

#[test]
fn a_target_below_the_native_rate_downsamples() {
    let target = StreamFormat {
        sample_rate: 24_000,
        channels: 1,
    };
    let (samples, format, _) = read(
        vec![
            Response::f32(&[0.0, 1.0, 2.0], 1, 48_000),
            Response::f32(&[3.0, 4.0, 5.0, 6.0, 7.0, 8.0], 1, 48_000),
        ],
        Some(target),
        3,
    );
    assert_eq!(samples, [0.0, 2.0, 4.0]);
    assert_eq!(format, target);
}

#[test]
fn stereo_folds_to_mono_before_resampling() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: 1,
    };
    let (samples, ..) = read(
        vec![Response::f32(
            &[0.0, 2.0, 1.0, 3.0, 2.0, 4.0, 3.0, 5.0],
            2,
            24_000,
        )],
        Some(target),
        4,
    );
    // Fold gives 1, 2, 3, 4; doubling the rate interpolates the midpoints.
    assert_eq!(samples, [1.0, 1.5, 2.0, 2.5]);
}

#[test]
fn mono_fans_out_to_the_widest_target_while_resampling() {
    let target = StreamFormat {
        sample_rate: 48_000,
        channels: MAX_TARGET_CHANNELS,
    };
    let (samples, format, _) = read(vec![Response::f32(&[0.0, 1.0], 1, 24_000)], Some(target), 2);
    assert_eq!(format, target);
    assert_eq!(samples.len(), 2 * MAX_TARGET_CHANNELS as usize);
    for (frame, expected) in samples
        .chunks_exact(MAX_TARGET_CHANNELS as usize)
        .zip([0.0, 0.5])
    {
        assert!(frame.iter().all(|sample| *sample == expected), "{frame:?}");
    }
}

#[test]
fn prepared_storage_accepts_variable_reads_within_its_budget() {
    with_player(
        vec![
            Response::f32(&[0.1, 0.2], 1, 48_000),
            Response::f32(&[0.3; 64], 1, 48_000),
        ],
        None,
        |player| {
            assert_eq!(player.read(2).expect("read").frames(), 2);
            assert_eq!(player.read(64).expect("read").frames(), 64);
        },
    );
}

#[test]
fn reads_cannot_exceed_the_prepared_budget() {
    script(Vec::new());
    with_plugin(|plugin| {
        let player = plugin
            .open("song.mod", 0)
            .expect("open")
            .prepare(4)
            .expect("prepare");
        let mut player = player;
        assert_eq!(
            player.read(5).expect_err("oversized read"),
            SessionError::FrameBudgetExceeded {
                requested: 5,
                prepared: 4,
            }
        );
    });
    assert!(calls().is_empty(), "an oversized read reached the plugin");
}

#[test]
fn preparation_rejects_a_budget_the_abi_cannot_advertise() {
    script(Vec::new());
    with_plugin(|plugin| {
        let error = plugin
            .open("song.mod", 0)
            .expect("open")
            .prepare(MAX_PREPARED_FRAMES + 1)
            .err()
            .expect("invalid budget");
        assert_eq!(
            error,
            SessionError::InvalidFrameBudget {
                requested: MAX_PREPARED_FRAMES + 1,
                maximum: MAX_PREPARED_FRAMES,
            }
        );
    });
}

#[test]
fn buffer_allocation_failure_is_reported() {
    let mut buffer = Vec::<u32>::new();
    assert_eq!(
        grow(&mut buffer, usize::MAX, "test"),
        Err(SessionError::Allocation("test"))
    );
}
