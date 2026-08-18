//! Dlopen coverage transferred from retrovert-core's v2 visualization suite.

use std::path::{Path, PathBuf};

use retrovert_host::ffi::playback::{RVPlaybackPlugin, RVScrollMode, RVVizCaps};
use retrovert_host::loader::{load_plugins, LoadedPlugin, PluginKind};
use retrovert_host::service::ServiceHost;
use retrovert_host::session::Plugin;
use retrovert_host::visualization::VisualizationConfig;

struct Case {
    name: &'static str,
    plugin_env: &'static str,
    plugin_path: &'static str,
    module_env: &'static str,
    module_path: &'static str,
    caps: u32,
    scroll_mode: Option<RVScrollMode>,
}

fn resolved(env: &str, relative: &str) -> PathBuf {
    std::env::var_os(env).map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative),
        PathBuf::from,
    )
}

fn load_test_plugin(path: &Path) -> retrovert_host::loader::LoadReport {
    load_plugins([path])
}

fn start_test_plugin<'a>(loaded: &'a mut LoadedPlugin, host: &'a ServiceHost) -> Plugin<'a> {
    Plugin::new(loaded, host).expect("plugin lifecycle")
}

fn exercise(case: Case) {
    let plugin_path = resolved(case.plugin_env, case.plugin_path);
    let module_path = resolved(case.module_env, case.module_path);
    assert!(
        plugin_path.exists(),
        "{}: plugin {} is not built",
        case.name,
        plugin_path.display()
    );
    assert!(
        module_path.exists(),
        "{}: module {} is missing",
        case.name,
        module_path.display()
    );

    let mut report = load_test_plugin(&plugin_path);
    assert!(
        report.errors.is_empty(),
        "{}: {:?}",
        case.name,
        report.errors
    );
    let loaded = report
        .plugins
        .of_kind_mut(PluginKind::Playback)
        .next()
        .expect("playback descriptor");
    let host = ServiceHost::default();
    let plugin = start_test_plugin(loaded, &host);
    let player = plugin
        .open(module_path.to_str().expect("UTF-8 module path"), 0)
        .expect("open module");
    let mut player = player.prepare(1_024).expect("prepare audio");

    player.set_scope_enabled(true);
    for _ in 0..8 {
        if player.read(1_024).expect("decode").finished {
            break;
        }
    }

    let layout = player
        .prepare_visualization(VisualizationConfig {
            scope_sample_budget: 257,
            ..VisualizationConfig::default()
        })
        .expect("valid visualization")
        .expect("visualization surface");
    let mut snapshot = layout.new_snapshot().expect("snapshot storage");
    player
        .capture_visualization(8_192, &mut snapshot)
        .expect("visualization capture");
    fn assert_send<T: Send>(_: &T) {}
    assert_send(&snapshot);

    assert_eq!(snapshot.output_frame, 8_192);
    assert_eq!(layout.caps & case.caps, case.caps, "{} caps", case.name);
    if let Some(scroll_mode) = case.scroll_mode {
        assert_eq!(layout.scroll_mode, scroll_mode, "{} scroll mode", case.name);
    }

    if case.caps & RVVizCaps::PATTERN_CELLS != 0 {
        assert!(!layout.columns.is_empty(), "{} columns", case.name);
        assert!(
            !layout.pattern_channels.is_empty(),
            "{} pattern channels",
            case.name
        );
        let position = snapshot.position.expect("pattern position");
        let rows = position.window_hi.saturating_sub(position.window_lo) as usize;
        if matches!(case.name, "libvgm" | "tfmx") {
            assert_eq!(rows, 64, "{} bounded normalized window", case.name);
            assert!(
                snapshot
                    .channel_rows()
                    .iter()
                    .all(|row| *row == position.row),
                "{} per-channel playheads share the normalized center",
                case.name
            );
        }
        assert_eq!(
            snapshot.cells().len(),
            rows * layout.pattern_channels.len() * layout.columns.len(),
            "{} complete advertised cell window",
            case.name
        );
        assert!(
            snapshot
                .cells()
                .iter()
                .any(|cell| cell.text.iter().any(|&byte| byte != 0)),
            "{} plugin-rendered cell text",
            case.name
        );
    } else {
        assert!(
            layout.columns.is_empty(),
            "{} unexpected columns",
            case.name
        );
        assert!(
            layout.pattern_channels.is_empty(),
            "{} unexpected pattern channels",
            case.name
        );
        assert!(
            snapshot.cells().is_empty(),
            "{} unexpected cells",
            case.name
        );
    }

    if case.caps & RVVizCaps::SCOPE != 0 {
        assert!(
            !layout.scope_channels.is_empty(),
            "{} scope channels",
            case.name
        );
        assert_eq!(
            snapshot.scope_counts().len(),
            layout.scope_channels.len(),
            "{} scope waveforms",
            case.name
        );
        assert!(
            (0..layout.scope_channels.len()).all(|channel| snapshot
                .scope(channel)
                .is_some_and(|waveform| waveform.len() <= 257)),
            "{} scope budget",
            case.name
        );
    } else {
        assert!(
            layout.scope_channels.is_empty(),
            "{} unexpected scope channels",
            case.name
        );
        assert!(
            snapshot.scope_counts().is_empty(),
            "{} unexpected scope",
            case.name
        );
    }

    if case.caps & RVVizCaps::VU != 0 {
        assert_eq!(
            snapshot.vu().len(),
            layout.scope_channels.len(),
            "{} plugin VU",
            case.name
        );
    } else {
        assert!(snapshot.vu().is_empty(), "{} unexpected VU", case.name);
    }
}

fn decode_prefix(plugin_path: &Path, module_path: &Path, budget: u32, frames: usize) -> Vec<f32> {
    let mut report = load_test_plugin(plugin_path);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    let loaded = report
        .plugins
        .of_kind_mut(PluginKind::Playback)
        .next()
        .expect("playback descriptor");
    let host = ServiceHost::default();
    let plugin = start_test_plugin(loaded, &host);
    let player = plugin
        .open(module_path.to_str().expect("UTF-8 module path"), 0)
        .expect("open module");
    let mut player = player.prepare(budget).expect("prepare audio");
    let mut samples = Vec::new();

    while samples.len() < frames {
        let chunk = player.read(budget).expect("decode");
        samples.extend_from_slice(chunk.samples);
        if chunk.finished || chunk.samples.is_empty() {
            break;
        }
    }
    samples.truncate(frames);
    samples
}

fn assert_small_reads_preserve_audio(
    plugin_env: &str,
    plugin_path: &str,
    module_env: &str,
    module_path: &str,
    frames: usize,
) {
    let plugin_path = resolved(plugin_env, plugin_path);
    let module_path = resolved(module_env, module_path);
    let whole = decode_prefix(&plugin_path, &module_path, frames as u32, frames);
    let one_at_a_time = decode_prefix(&plugin_path, &module_path, 1, frames);
    assert_eq!(
        one_at_a_time, whole,
        "small reads must preserve every decoded frame"
    );
}

macro_rules! viz_case {
    (
        $test:ident, $name:literal, $plugin_env:literal, $plugin_path:literal,
        $module_env:literal, $module_path:literal, $caps:expr, $scroll:expr
    ) => {
        #[test]
        #[ignore = "requires playback plugin and module fixtures"]
        fn $test() {
            exercise(Case {
                name: $name,
                plugin_env: $plugin_env,
                plugin_path: $plugin_path,
                module_env: $module_env,
                module_path: $module_path,
                caps: $caps,
                scroll_mode: $scroll,
            });
        }
    };
}

const SCOPE: u32 = RVVizCaps::SCOPE;
const PATTERN_SCOPE: u32 = RVVizCaps::PATTERN_CELLS | RVVizCaps::SCOPE;

viz_case!(
    adplug,
    "adplug",
    "RV_ADPLUG_SO",
    "../../../playback-adplug/build/plugins/adplug_playback.so",
    "RV_ADPLUG_MODULE",
    "../../../../replay_frontend/data/test_data/music/adplug/test.hsc",
    SCOPE,
    None
);

#[test]
#[ignore = "requires playback plugin and module fixtures"]
fn ixalance_small_reads_preserve_audio() {
    assert_small_reads_preserve_audio(
        "RV_IXALANCE_SO",
        "../../../playback-ixalance/build/plugins/ixalance_playback.so",
        "RV_IXALANCE_MODULE",
        "../../../../replay_frontend/data/test_data/music/ixalance/test.ixs",
        113,
    );
}

#[test]
#[ignore = "requires playback plugin and module fixtures"]
fn spu_small_reads_preserve_audio() {
    assert_small_reads_preserve_audio(
        "RV_SPU_SO",
        "../../../playback-spu/build/plugins/spu_playback.so",
        "RV_SPU_MODULE",
        "../../../../replay_frontend/data/test_data/music/spu/test.spu",
        113,
    );
}
viz_case!(
    art_of_noise,
    "art-of-noise",
    "RV_AON_SO",
    "../../../playback-art-of-noise/build/plugins/art_of_noise_playback.so",
    "RV_AON_MODULE",
    "../../../../replay_frontend/data/test_data/music/art_of_noise/test.aon",
    PATTERN_SCOPE,
    Some(RVScrollMode::Synchronized)
);
viz_case!(
    audio_stream,
    "audio-stream",
    "RV_AUDIO_STREAM_SO",
    "../../../playback-audio_stream/build/plugins/audio_stream_playback.so",
    "RV_AUDIO_STREAM_MODULE",
    "../../../../replay_frontend/data/test_data/music/audio_stream/test.ogg",
    SCOPE,
    None
);
viz_case!(
    furnace,
    "furnace",
    "RV_FURNACE_SO",
    "../../../playback-furnace/build/plugins/furnace_playback.so",
    "RV_FURNACE_MODULE",
    "../../../../replay_frontend/data/test_data/music/furnace/test.fur",
    SCOPE,
    None
);
viz_case!(
    hively,
    "hively",
    "RV_HIVELY_SO",
    "../../../playback-hively/build/plugins/hively_playback.so",
    "RV_HIVELY_MODULE",
    "../../../../replay_frontend/data/test_data/music/hively/2_much_pressure.ahx",
    PATTERN_SCOPE,
    Some(RVScrollMode::Synchronized)
);
viz_case!(
    ixalance,
    "ixalance",
    "RV_IXALANCE_SO",
    "../../../playback-ixalance/build/plugins/ixalance_playback.so",
    "RV_IXALANCE_MODULE",
    "../../../../replay_frontend/data/test_data/music/ixalance/test.ixs",
    PATTERN_SCOPE,
    Some(RVScrollMode::Synchronized)
);
viz_case!(
    klystrack,
    "klystrack",
    "RV_KLYSTRACK_SO",
    "../../../playback-klystrack/build/plugins/klystrack_playback.so",
    "RV_KLYSTRACK_MODULE",
    "../../../../replay_frontend/data/test_data/music/klystrack/test.kt",
    SCOPE,
    None
);
viz_case!(
    libvgm,
    "libvgm",
    "RV_LIBVGM_SO",
    "../../../playback-libvgm/build/plugins/libvgm_playback.so",
    "RV_LIBVGM_MODULE",
    "../../../../replay_frontend/data/test_data/music/libvgm/earthworm_jim.vgm",
    PATTERN_SCOPE,
    Some(RVScrollMode::PerChannel)
);
viz_case!(
    mdxmini,
    "mdxmini",
    "RV_MDXMINI_SO",
    "../../../playback-mdxmini/build/plugins/mdxmini_playback.so",
    "RV_MDXMINI_MODULE",
    "../../../../replay_frontend/data/test_data/music/mdxmini/test.mdx",
    SCOPE,
    None
);
viz_case!(
    openmpt,
    "openmpt",
    "RV_OPENMPT_SO",
    "../../../playback-openmpt/build/plugins/openmpt_playback.so",
    "RV_OPENMPT_MODULE",
    "../../../../replay_frontend/data/test_data/music/openmpt/test.amf",
    PATTERN_SCOPE,
    Some(RVScrollMode::Synchronized)
);
viz_case!(
    organya,
    "organya",
    "RV_ORGANYA_SO",
    "../../../playback-organya/build/plugins/organya_playback.so",
    "RV_ORGANYA_MODULE",
    "../../../../replay_frontend/data/test_data/music/organya/test.org",
    SCOPE,
    None
);
viz_case!(
    pmdmini,
    "pmdmini",
    "RV_PMDMINI_SO",
    "../../../playback-pmdmini/build/plugins/pmdmini_playback.so",
    "RV_PMDMINI_MODULE",
    "../../../../replay_frontend/data/test_data/music/pmdmini/test.m",
    SCOPE,
    None
);
viz_case!(
    pxtone,
    "pxtone",
    "RV_PXTONE_SO",
    "../../../playback-pxtone/build/plugins/pxtone_playback.so",
    "RV_PXTONE_MODULE",
    "../../../../replay_frontend/data/test_data/music/pxtone/test.ptcop",
    SCOPE | RVVizCaps::VU,
    Some(RVScrollMode::PerChannel)
);
viz_case!(
    sc68,
    "sc68",
    "RV_SC68_SO",
    "../../../playback-sc68/build/plugins/sc68_playback.so",
    "RV_SC68_MODULE",
    "../../../../replay_frontend/data/test_data/music/sc68/shadow_of_the_beast.sc68",
    SCOPE,
    None
);
viz_case!(
    sidplayfp,
    "sidplayfp",
    "RV_SIDPLAYFP_SO",
    "../../../playback-sidplayfp/build/plugins/sidplayfp_playback.so",
    "RV_SID_MODULE",
    "../../../../replay_frontend/data/test_data/music/sidplayfp/artefacts.sid",
    SCOPE,
    None
);
viz_case!(
    spu,
    "spu",
    "RV_SPU_SO",
    "../../../playback-spu/build/plugins/spu_playback.so",
    "RV_SPU_MODULE",
    "../../../../replay_frontend/data/test_data/music/spu/test.spu",
    SCOPE,
    None
);
viz_case!(
    tfmx,
    "tfmx",
    "RV_TFMX_SO",
    "../../../playback-tfmx/build/plugins/tfmx_playback.so",
    "RV_TFMX_MODULE",
    "../../../../replay_frontend/data/test_data/music/tfmx/mdat.lovely_feelings",
    PATTERN_SCOPE,
    Some(RVScrollMode::PerChannel)
);
viz_case!(
    uade,
    "uade",
    "RV_UADE_SO",
    "../../../playback-uade/build/plugins/uade_playback.so",
    "RV_UADE_MODULE",
    "../../../../replay_frontend/data/test_data/music/uade/expressions.bp",
    SCOPE,
    None
);
viz_case!(
    v2m,
    "v2m",
    "RV_V2M_SO",
    "../../../playback-v2m/build/plugins/v2m_playback.so",
    "RV_V2M_MODULE",
    "../../../../replay_frontend/data/test_data/music/v2m/test.v2m",
    PATTERN_SCOPE | RVVizCaps::VU,
    Some(RVScrollMode::Synchronized)
);
viz_case!(
    xsf,
    "xsf",
    "RV_XSF_SO",
    "../../../playback-xsf/build/plugins/xsf_playback.so",
    "RV_XSF_MODULE",
    "../../../../replay_frontend/data/test_data/music/xsf/test.psf",
    SCOPE,
    None
);
viz_case!(
    xsf_gsf_without_visualization,
    "xsf-gsf",
    "RV_XSF_SO",
    "../../../playback-xsf/build/plugins/xsf_playback.so",
    "RV_XSF_GSF_MODULE",
    "../../../../replay_frontend/data/test_data/music/xsf/test.gsf",
    0,
    None
);
viz_case!(
    zxtune,
    "zxtune",
    "RV_ZXTUNE_SO",
    "../../../playback-zxtune/build/plugins/zxtune_playback.so",
    "RV_ZXTUNE_MODULE",
    "../../../../replay_frontend/data/test_data/music/zxtune/test.emul",
    SCOPE,
    None
);

#[test]
#[ignore = "requires playback plugin binaries"]
fn no_visualization_plugins_keep_every_viz_slot_null() {
    const PLUGINS: &[(&str, &str, &str)] = &[
        (
            "asap",
            "RV_ASAP_SO",
            "../../../playback-asap/build/plugins/asap_playback.so",
        ),
        (
            "cpsycle",
            "RV_CPSYCLE_SO",
            "../../../playback-cpsycle/build/plugins/cpsycle_playback.so",
        ),
        (
            "gme",
            "RV_GME_SO",
            "../../../playback-gme/build/plugins/gme_playback.so",
        ),
        (
            "eupmini",
            "RV_EUPMINI_SO",
            "../../../playback-eupmini/build/plugins/eupmini_playback.so",
        ),
        (
            "fmplayer",
            "RV_FMPLAYER_SO",
            "../../../playback-fmplayer/build/plugins/fmplayer_playback.so",
        ),
        (
            "libkss",
            "RV_LIBKSS_SO",
            "../../../playback-libkss/build/plugins/libkss_playback.so",
        ),
        (
            "sunvox",
            "RV_SUNVOX_SO",
            "../../../playback-sunvox/build/plugins/sunvox_playback.so",
        ),
    ];

    for (name, plugin_env, plugin_relative) in PLUGINS {
        let path = resolved(plugin_env, plugin_relative);
        assert!(
            path.exists(),
            "{name}: plugin {} is not built",
            path.display()
        );
        let report = load_test_plugin(&path);
        assert!(report.errors.is_empty(), "{name}: {:?}", report.errors);
        let plugin: &RVPlaybackPlugin = report
            .plugins
            .of_kind(PluginKind::Playback)
            .next()
            .and_then(|loaded| loaded.playback())
            .expect("playback descriptor");
        assert!(plugin.viz_info.is_none(), "{name}: viz_info");
        assert!(plugin.tracker_columns.is_none(), "{name}: tracker_columns");
        assert!(
            plugin.tracker_channels.is_none(),
            "{name}: tracker_channels"
        );
        assert!(plugin.scope_channels.is_none(), "{name}: scope_channels");
        assert!(
            plugin.tracker_position.is_none(),
            "{name}: tracker_position"
        );
        assert!(
            plugin.tracker_channel_rows.is_none(),
            "{name}: tracker_channel_rows"
        );
        assert!(plugin.tracker_cells.is_none(), "{name}: tracker_cells");
        assert!(plugin.scope_enable.is_none(), "{name}: scope_enable");
        assert!(plugin.scope_samples.is_none(), "{name}: scope_samples");
        assert!(plugin.vu_levels.is_none(), "{name}: vu_levels");
    }
}
