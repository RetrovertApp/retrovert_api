//! End-to-end coverage against a real C playback plugin built from the headers.

use super::*;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::c_fixture::compile;
use crate::loader::{load_plugins, LoadReport, LoadedPlugin, PluginKind};
use crate::service::{Io, Log, LogLevel, MemorySettingsStore, StdFsIo};

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

const OWNED_FIXTURE: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <retrovert/playback.h>

#define OWNED_NAME "owned"
#define STATIC_EVENTS ""

typedef struct Instance { char events[1024]; } Instance;
static char last_events[1024];

static void note(const char* path, const char* event) {
    if (path[0] == '\0') return;
    FILE* file = fopen(path, "a");
    if (file == NULL) return;
    fprintf(file, "%s:%s\n", OWNED_NAME, event);
    fclose(file);
}

static RVProbeResult probe(uint8_t* data, uint64_t size, const char* name, uint64_t total) {
    (void)data; (void)size; (void)total;
    return strstr(name, OWNED_NAME) == NULL ? RVProbeResult_Unsupported : RVProbeResult_Supported;
}

static void static_init(const RVService* services) {
    (void)services;
    note(STATIC_EVENTS, "static_init");
}
static void static_destroy(void) { note(last_events, "static_destroy"); }

static void* create(const RVService* services) {
    (void)services;
    return calloc(1, sizeof(Instance));
}

static int32_t destroy(void* user_data) {
    Instance* instance = (Instance*)user_data;
    note(instance->events, "destroy");
    free(instance);
    return 0;
}

static int32_t open_song(void* user_data, const char* url, uint32_t subsong, const RVService* services) {
    (void)subsong; (void)services;
    Instance* instance = (Instance*)user_data;
    snprintf(instance->events, sizeof(instance->events), "%s", url);
    snprintf(last_events, sizeof(last_events), "%s", url);
    note(instance->events, "open");
    return strstr(url, "missing") == NULL ? 0 : -7;
}

static void close_song(void* user_data) {
    Instance* instance = (Instance*)user_data;
    note(instance->events, "close");
}

static RVReadInfo read_data(void* user_data, RVReadData data) {
    (void)user_data;
    uint32_t frames = data.info.frame_count;
    if (frames > data.channels_output_max_bytes_size / sizeof(float)) {
        frames = data.channels_output_max_bytes_size / sizeof(float);
    }
    float* output = (float*)data.channels_output;
    for (uint32_t i = 0; i < frames; i++) output[i] = (float)i + 0.25f;
    RVReadInfo info;
    info.format.audio_format = RVAudioStreamFormat_F32;
    info.format.channel_count = 1;
    info.format.sample_rate = 48000;
    info.frame_count = frames;
    info.status = RVReadStatus_Ok;
    return info;
}

static int32_t metadata(const char* url, const RVService* services) {
    (void)services;
    return strstr(url, "bad-metadata") == NULL ? 0 : -9;
}

static bool viz_info(void* user_data, RVVizInfo* out) {
    (void)user_data;
    memset(out, 0, sizeof(*out));
    out->caps = RVVizCaps_Scope;
    out->scroll_mode = RVScrollMode_Synchronized;
    out->scope_channel_count = 1;
    return true;
}

static uint32_t scope_channels(void* user_data, RVChannelDesc* out, uint32_t cap) {
    (void)user_data;
    if (cap == 0) return 0;
    memset(out, 0, sizeof(*out));
    memcpy(out->name, "mono", 4);
    out->scope_width = 1;
    return 1;
}

static uint32_t scope_samples(void* user_data, int32_t channel, float* out, uint32_t cap) {
    (void)user_data; (void)channel;
    for (uint32_t i = 0; i < cap; i++) out[i] = (float)i;
    return cap;
}

static RVPlaybackPlugin plugin = {
    .api_version = RV_PLAYBACK_PLUGIN_API_VERSION,
    .name = OWNED_NAME, .version = "1.0", .library_version = "2.0",
    .probe_can_play = probe, .create = create, .destroy = destroy,
    .open = open_song, .close = close_song, .read_data = read_data,
    .metadata = metadata,
    .static_init = static_init, .static_destroy = static_destroy,
    .viz_info = viz_info, .scope_channels = scope_channels,
    .scope_samples = scope_samples,
};

RVPlaybackPlugin* rv_playback_plugin(void) { return &plugin; }

__attribute__((destructor)) static void library_unload(void) {
    note(last_events, "library_unload");
}
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

struct DropLoggingIo {
    events: PathBuf,
}

impl Io for DropLoggingIo {
    fn exists(&self, url: &str) -> bool {
        Path::new(url).exists()
    }

    fn read_to_memory(&self, url: &str) -> Option<Vec<u8>> {
        std::fs::read(url).ok()
    }
}

impl Drop for DropLoggingIo {
    fn drop(&mut self) {
        use std::io::Write;

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events)
            .expect("event log");
        writeln!(file, "service_host_destroy").expect("event line");
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

fn owned_fixture(
    dir: &tempfile::TempDir,
    event_name: &str,
) -> (OwnedPlaybackRuntime, OwnedPlaybackPlugin, PathBuf) {
    compile(dir.path(), "owned_session_fixture", OWNED_FIXTURE);
    let report = load_plugins([dir.path()]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let events = dir.path().join(event_name);
    let host = ServiceHost::new(
        Box::new(DropLoggingIo {
            events: events.clone(),
        }),
        Arc::new(CapturingLog::default()),
        Box::new(MemorySettingsStore::default()),
    );
    let runtime = OwnedPlaybackRuntime::new(report.plugins, host);
    let plugin = runtime
        .select(&[1], "owned.mod", 1)
        .expect("owned playback plugin");
    (runtime, plugin, events)
}

fn event_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("event log")
        .lines()
        .map(|line| line.strip_prefix("owned:").unwrap_or(line).to_owned())
        .collect()
}

fn c_string_literal(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
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
fn owned_session_closes_and_destroys_before_runtime_teardown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (runtime, plugin, events) = owned_fixture(&dir, "events");
    let mut session = plugin
        .open_prepared(events.to_str().expect("event path"), 0, 8, None)
        .expect("owned session");
    let chunk = session.read(2).expect("owned read");
    assert_eq!(chunk.frames(), 2);
    assert_eq!(chunk.samples, [0.25, 1.25]);

    drop(runtime);
    drop(plugin);
    assert_eq!(event_lines(&events), ["open"]);
    drop(session);

    assert_eq!(
        event_lines(&events),
        [
            "open",
            "close",
            "destroy",
            "static_destroy",
            "library_unload",
            "service_host_destroy",
        ]
    );
}

#[test]
fn owned_failed_open_destroys_its_instance() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (runtime, plugin, events) = owned_fixture(&dir, "missing-events");

    assert!(matches!(
        plugin.open_prepared(events.to_str().expect("event path"), 0, 8, None),
        Err(OwnedSessionError::Session(SessionError::OpenFailed(-7)))
    ));
    assert_eq!(event_lines(&events), ["open", "destroy"]);
    drop(plugin);
    drop(runtime);

    assert_eq!(
        event_lines(&events),
        [
            "open",
            "destroy",
            "static_destroy",
            "library_unload",
            "service_host_destroy",
        ]
    );
}

#[test]
fn owned_replacement_cleanup_precedes_the_next_open() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (runtime, plugin, events) = owned_fixture(&dir, "replacement-events");
    let mut session = Some(
        plugin
            .open_prepared(events.to_str().expect("event path"), 0, 8, None)
            .expect("first session"),
    );

    plugin
        .replace_prepared(
            &mut session,
            events.to_str().expect("event path"),
            1,
            8,
            None,
        )
        .expect("replacement session");
    assert_eq!(event_lines(&events), ["open", "close", "destroy", "open"]);

    let missing = dir.path().join("missing-replacement-events");
    assert!(matches!(
        plugin.replace_prepared(
            &mut session,
            missing.to_str().expect("missing path"),
            2,
            8,
            None,
        ),
        Err(OwnedSessionError::Session(SessionError::OpenFailed(-7)))
    ));
    assert!(session.is_none());
    assert_eq!(
        event_lines(&events),
        ["open", "close", "destroy", "open", "close", "destroy"]
    );
    assert_eq!(event_lines(&missing), ["open", "destroy"]);

    drop(session);
    drop(plugin);
    drop(runtime);
}

#[test]
fn owned_visualization_reuses_prepared_slots_without_allocation() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (runtime, plugin, events) = owned_fixture(&dir, "visualization-events");
    let mut session = plugin
        .open_prepared(
            events.to_str().expect("event path"),
            0,
            8,
            Some(VisualizationConfig {
                scope_sample_budget: 8,
                pattern_row_budget: 4,
            }),
        )
        .expect("owned session");
    let mut first = session
        .layout()
        .expect("visualization layout")
        .new_snapshot()
        .expect("first prepared slot");
    let mut second = session
        .layout()
        .expect("visualization layout")
        .new_snapshot()
        .expect("second prepared slot");

    crate::test_alloc::assert_no_alloc(|| {
        session
            .capture_visualization(1, &mut first)
            .expect("first capture");
        session
            .capture_visualization(2, &mut second)
            .expect("second capture");
    });

    assert_eq!(first.output_frame, 1);
    assert_eq!(second.output_frame, 2);
    assert_eq!(first.scope_counts(), [8]);
    assert_eq!(
        first.scope(0).expect("first scope"),
        [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
    );
    assert_eq!(second.scope_counts(), [8]);
    assert_eq!(
        second.scope(0).expect("second scope"),
        [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
    );
    drop(session);
    drop(plugin);
    drop(runtime);
}

#[test]
fn owned_metadata_covers_success_and_failure() {
    let dir = tempfile::tempdir().expect("temp dir");
    let (runtime, plugin, _events) = owned_fixture(&dir, "metadata-events");

    assert!(plugin
        .metadata("song.mod")
        .expect("metadata success")
        .is_some());
    assert_eq!(
        plugin.metadata("bad-metadata.mod").err(),
        Some(SessionError::MetadataFailed(-9))
    );

    drop(plugin);
    drop(runtime);
}

#[test]
fn every_owned_catalog_plugin_initializes_and_tears_down_once() {
    let dir = tempfile::tempdir().expect("temp dir");
    let events = dir.path().join("catalog-events");
    let static_events = c_string_literal(&events);
    let alpha = OWNED_FIXTURE
        .replace(
            "#define OWNED_NAME \"owned\"",
            "#define OWNED_NAME \"alpha\"",
        )
        .replace(
            "#define STATIC_EVENTS \"\"",
            &format!("#define STATIC_EVENTS \"{static_events}\""),
        );
    let beta = OWNED_FIXTURE
        .replace(
            "#define OWNED_NAME \"owned\"",
            "#define OWNED_NAME \"beta\"",
        )
        .replace(
            "#define STATIC_EVENTS \"\"",
            &format!("#define STATIC_EVENTS \"{static_events}\""),
        );
    compile(dir.path(), "alpha_fixture", &alpha);
    compile(dir.path(), "beta_fixture", &beta);
    let report = load_plugins([dir.path()]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let host = ServiceHost::new(
        Box::new(DropLoggingIo {
            events: events.clone(),
        }),
        Arc::new(CapturingLog::default()),
        Box::new(MemorySettingsStore::default()),
    );
    let runtime = OwnedPlaybackRuntime::new(report.plugins, host);
    let alpha = runtime.select(&[1], "alpha.mod", 1).expect("alpha plugin");
    let beta = runtime.select(&[1], "beta.mod", 1).expect("beta plugin");
    let alpha_session = alpha
        .open_prepared(events.to_str().expect("event path"), 0, 8, None)
        .expect("alpha session");
    let beta_session = beta
        .open_prepared(events.to_str().expect("event path"), 0, 8, None)
        .expect("beta session");

    drop((alpha_session, beta_session, alpha, beta, runtime));

    let lines = event_lines(&events);
    for plugin in ["alpha", "beta"] {
        for event in ["static_init", "static_destroy", "library_unload"] {
            let tagged = format!("{plugin}:{event}");
            assert_eq!(lines.iter().filter(|line| *line == &tagged).count(), 1);
        }
    }
    assert_eq!(
        lines.last().map(String::as_str),
        Some("service_host_destroy")
    );
}

#[test]
fn owned_runtime_accepts_optional_static_teardown() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = OWNED_FIXTURE.replace(
        ".static_init = static_init, .static_destroy = static_destroy,",
        ".static_init = static_init, .static_destroy = NULL,",
    );
    compile(dir.path(), "optional_teardown_fixture", &source);
    let report = load_plugins([dir.path()]);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let events = dir.path().join("optional-events");
    let host = ServiceHost::new(
        Box::new(DropLoggingIo {
            events: events.clone(),
        }),
        Arc::new(CapturingLog::default()),
        Box::new(MemorySettingsStore::default()),
    );
    let runtime = OwnedPlaybackRuntime::new(report.plugins, host);
    let plugin = runtime.select(&[1], "owned.mod", 1).expect("owned plugin");
    let session = plugin
        .open_prepared(events.to_str().expect("event path"), 0, 8, None)
        .expect("owned session");

    drop((session, plugin, runtime));

    assert_eq!(
        event_lines(&events),
        [
            "open",
            "close",
            "destroy",
            "library_unload",
            "service_host_destroy",
        ]
    );
}

fn simultaneous_owned_sessions_drop_in_order(reverse: bool) {
    let dir = tempfile::tempdir().expect("temp dir");
    let event_name = if reverse {
        "reverse-events"
    } else {
        "forward-events"
    };
    let (runtime, plugin, events) = owned_fixture(&dir, event_name);
    let first = plugin
        .open_prepared(events.to_str().expect("event path"), 0, 8, None)
        .expect("first session");
    let second = plugin
        .open_prepared(events.to_str().expect("event path"), 1, 8, None)
        .expect("second session");
    drop(runtime);
    drop(plugin);

    if reverse {
        drop(second);
        assert_eq!(event_lines(&events).len(), 4);
        drop(first);
    } else {
        drop(first);
        assert_eq!(event_lines(&events).len(), 4);
        drop(second);
    }

    let lines = event_lines(&events);
    assert_eq!(lines.iter().filter(|line| *line == "close").count(), 2);
    assert_eq!(lines.iter().filter(|line| *line == "destroy").count(), 2);
    assert_eq!(
        &lines[lines.len() - 3..],
        ["static_destroy", "library_unload", "service_host_destroy"]
    );
}

#[test]
fn simultaneous_owned_sessions_drop_forward() {
    simultaneous_owned_sessions_drop_in_order(false);
}

#[test]
fn simultaneous_owned_sessions_drop_reverse() {
    simultaneous_owned_sessions_drop_in_order(true);
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
