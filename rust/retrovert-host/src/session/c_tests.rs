//! End-to-end coverage against a real C playback plugin built from the headers.

use super::*;

use std::sync::{Arc, Mutex};

use crate::c_fixture::compile;
use crate::loader::{load_plugins, LoadReport, LoadedPlugin, PluginKind};
use crate::service::{Log, LogLevel, MemorySettingsStore, StdFsIo};

/// A plugin that logs on every read and answers with a rising 22050 Hz mono ramp, so a
/// session over it exercises normalization, the service vtables and the ABI ordering.
const FIXTURE: &str = r#"
#include <stdlib.h>
#include <string.h>

#include <retrovert/log.h>
#include <retrovert/playback.h>
#include <retrovert/service.h>

RV_PLUGIN_USE_LOG_API();

typedef struct Instance {
    int16_t next;
} Instance;

static int inits;
int fixture_inits(void) { return inits; }

static void static_init(const RVService* services) {
    rv_init_log_api(services);
    inits++;
}

static void static_destroy(void) { inits--; }

static void* create(const RVService* services) {
    (void)services;
    return calloc(1, sizeof(Instance));
}

static int32_t destroy(void* user_data) {
    free(user_data);
    return 0;
}

static int32_t open_song(void* user_data, const char* url, uint32_t subsong, const RVService* services) {
    (void)services;
    Instance* instance = (Instance*)user_data;
    if (strstr(url, "missing") != NULL) {
        return -7;
    }
    instance->next = (int16_t)subsong;
    return 0;
}

static void close_song(void* user_data) { (void)user_data; }

static RVReadInfo read_data(void* user_data, RVReadData dest) {
    Instance* instance = (Instance*)user_data;
    rvfl_info("read %u frames", dest.info.frame_count);

    uint32_t frames = dest.info.frame_count;
    if (frames > dest.channels_output_max_bytes_size / sizeof(int16_t)) {
        frames = dest.channels_output_max_bytes_size / sizeof(int16_t);
    }
    int16_t* out = (int16_t*)dest.channels_output;
    for (uint32_t i = 0; i < frames; i++) {
        out[i] = (int16_t)(instance->next++ * 256);
    }

    RVReadInfo info;
    info.format.audio_format = RVAudioStreamFormat_S16;
    info.format.channel_count = 1;
    info.format.sample_rate = 22050;
    info.frame_count = frames;
    info.status = RVReadStatus_Ok;
    return info;
}

static RVSettingsUpdate settings_updated(void* user_data, const RVService* services) {
    (void)user_data; (void)services;
    return RVSettingsUpdate_RequireRestart;
}

static RVPlaybackPlugin plugin = {
    .api_version = RV_PLAYBACK_PLUGIN_API_VERSION,
    .name = "fixture", .version = "1.0", .library_version = "2.0",
    .create = create, .destroy = destroy,
    .open = open_song, .close = close_song, .read_data = read_data,
    .static_init = static_init, .static_destroy = static_destroy,
    .settings_updated = settings_updated,
};

RVPlaybackPlugin* rv_playback_plugin(void) { return &plugin; }
"#;

#[derive(Default)]
struct CapturingLog {
    lines: Mutex<Vec<String>>,
}

impl Log for CapturingLog {
    fn log(&self, _level: LogLevel, _file: Option<&str>, _line: u32, message: &str) {
        self.lines
            .lock()
            .expect("log lines")
            .push(message.to_owned());
    }
}

struct PanickingLog;

impl Log for PanickingLog {
    fn log(&self, _level: LogLevel, _file: Option<&str>, _line: u32, _message: &str) {
        panic!("host log blew up")
    }
}

/// The fixture's init counter, read through a second handle on the same mapping.
fn fixture_inits(path: &std::path::Path) -> i32 {
    // SAFETY: the fixture is already loaded, so this resolves to the same mapping, and
    // its `fixture_inits` has the signature named here.
    unsafe {
        let library = libloading::Library::new(path).expect("reopen fixture");
        let inits: libloading::Symbol<'_, unsafe extern "C" fn() -> i32> =
            library.get(b"fixture_inits\0").expect("fixture_inits");
        inits()
    }
}

fn host(log: Arc<dyn Log>) -> ServiceHost {
    ServiceHost::new(
        Box::new(StdFsIo),
        log,
        Box::new(MemorySettingsStore::default()),
    )
}

/// Builds the fixture into a fresh directory and loads it.
fn load_fixture(dir: &tempfile::TempDir) -> LoadReport {
    compile(dir.path(), "session_fixture", FIXTURE);
    let report = load_plugins([dir.path()]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    report
}

fn playback(report: &mut LoadReport) -> &mut LoadedPlugin {
    report
        .plugins
        .of_kind_mut(PluginKind::Playback)
        .next()
        .expect("fixture loaded")
}

#[test]
fn a_c_plugin_plays_through_the_session() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut report = load_fixture(&dir);
    let loaded = playback(&mut report);
    let fixture_path = loaded.path().to_owned();

    let captured = Arc::new(CapturingLog::default());
    let host = host(captured.clone());
    {
        let plugin = Plugin::new(loaded, &host).expect("playback plugin");
        assert_eq!(fixture_inits(&fixture_path), 1, "static_init did not run");

        // Subsong 4 seeds the ramp, so the samples prove the argument reached the plugin.
        let mut player = plugin
            .open("song.mod", 4)
            .expect("open")
            .prepare(3)
            .expect("prepare");
        let chunk = player.read(3).expect("read");
        assert_eq!(
            chunk.samples,
            [
                4.0 * 256.0 / 32768.0,
                5.0 * 256.0 / 32768.0,
                6.0 * 256.0 / 32768.0
            ]
        );
        assert_eq!(
            chunk.format,
            StreamFormat {
                sample_rate: 22_050,
                channels: 1
            }
        );
        assert_eq!(
            player.native_format().expect("locked").sample_format,
            RVAudioStreamFormat::S16
        );
        assert_eq!(player.settings_updated(), RVSettingsUpdate::RequireRestart);

        // The plugin only refuses this url, so the failure is its own, not the loader's.
        assert_eq!(
            plugin.open("missing.mod", 0).err(),
            Some(SessionError::OpenFailed(-7))
        );
    }

    assert_eq!(
        fixture_inits(&fixture_path),
        0,
        "static_destroy did not run"
    );

    {
        let _plugin = Plugin::new(loaded, &host).expect("sequential plugin");
        assert_eq!(fixture_inits(&fixture_path), 1, "static_init did not rerun");
    }
    assert_eq!(
        fixture_inits(&fixture_path),
        0,
        "static_destroy did not rerun"
    );

    let lines = captured.lines.lock().expect("log lines");
    assert_eq!(lines.as_slice(), ["read 3 frames"]);
}

#[test]
fn a_target_format_reaches_the_mixer_ready() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut report = load_fixture(&dir);
    let loaded = playback(&mut report);

    let host = host(Arc::new(CapturingLog::default()));
    let plugin = Plugin::new(loaded, &host).expect("playback plugin");
    let target = StreamFormat {
        sample_rate: 44_100,
        channels: 2,
    };
    let mut player = plugin
        .open_with_target("song.mod", 0, target)
        .expect("open")
        .prepare(64)
        .expect("prepare");

    let chunk = player.read(64).expect("read");
    assert_eq!(chunk.frames(), 64);
    assert_eq!(chunk.format, target);
    // Mono fanned out, so both channels of every frame agree.
    for frame in chunk.samples.chunks_exact(2) {
        assert_eq!(frame[0], frame[1]);
    }
    assert_eq!(
        player.native_format().expect("locked").sample_rate,
        22_050,
        "the native rate is still reported under conversion"
    );
}

#[test]
fn a_plugin_of_another_kind_cannot_open_a_session() {
    const RESAMPLER: &str = r#"#include <retrovert/resample.h>
static RVResamplePlugin plugin = {
    .api_version = 2, .name = "converter", .version = "1.0", .library_version = "2.0",
};
RVResamplePlugin* rv_resample_plugin(void) { return &plugin; }
"#;

    let dir = tempfile::tempdir().expect("temp dir");
    compile(dir.path(), "resample_fixture", RESAMPLER);
    let mut report = load_plugins([dir.path()]);
    let loaded = report
        .plugins
        .of_kind_mut(PluginKind::Resample)
        .next()
        .expect("fixture loaded");

    let host = host(Arc::new(CapturingLog::default()));
    assert_eq!(
        Plugin::new(loaded, &host).err(),
        Some(SessionError::NotPlayback)
    );
}

#[test]
fn a_panicking_host_service_does_not_unwind_into_the_plugin() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut report = load_fixture(&dir);
    let loaded = playback(&mut report);

    let host = host(Arc::new(PanickingLog));
    let plugin = Plugin::new(loaded, &host).expect("playback plugin");
    let mut player = plugin
        .open("song.mod", 0)
        .expect("open")
        .prepare(2)
        .expect("prepare");

    let chunk = player.read(2).expect("read survives the panicking log");
    assert_eq!(chunk.frames(), 2);
}
