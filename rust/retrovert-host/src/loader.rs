//! Discovery and loading: paths in, validated plugins in probe order out.
//!
//! A path is either a file, loaded directly, or a directory, scanned non-recursively
//! for the extension canon. Acquisition — building, staging, downloading — stays with
//! the host; this module's world begins at shared libraries on disk.

use core::ffi::{c_char, c_void};
use core::fmt;
use core::mem::offset_of;
use core::ptr::NonNull;
use std::collections::HashSet;
use std::ffi::CString;
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use crate::ffi::output::RVOutputPlugin;
use crate::ffi::playback::{RVPlaybackPlugin, RVProbeResult, RV_PLAYBACK_PLUGIN_API_VERSION};
use crate::ffi::resample::RVResamplePlugin;
use crate::plugin_string::plugin_str;

/// Longest plugin name accepted from a descriptor, in bytes.
pub const PLUGIN_NAME_LIMIT: usize = 128;
/// Longest plugin version accepted from a descriptor, in bytes.
pub const PLUGIN_VERSION_LIMIT: usize = 64;
/// Longest underlying-library version accepted from a descriptor, in bytes.
pub const PLUGIN_LIBRARY_VERSION_LIMIT: usize = 256;

/// Accepted on every platform, so a plugin built elsewhere still loads here.
const PLUGIN_EXTENSIONS: [&str; 4] = ["so", "dylib", "dll", "rvp"];

/// One version covers the whole ABI; `playback.h` is the only header that spells it.
const PLUGIN_API_VERSION: u64 = RV_PLAYBACK_PLUGIN_API_VERSION;

/// The three descriptors a library can publish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginKind {
    /// A decoder that probes and reads media.
    Playback,
    /// An audio-output backend.
    Output,
    /// A sample-rate conversion backend.
    Resample,
}

impl PluginKind {
    const ALL: [Self; 3] = [Self::Playback, Self::Output, Self::Resample];

    /// The entry-point symbol, NUL-terminated for `dlsym`.
    const fn symbol(self) -> &'static [u8] {
        match self {
            Self::Playback => b"rv_playback_plugin\0",
            Self::Output => b"rv_output_plugin\0",
            Self::Resample => b"rv_resample_plugin\0",
        }
    }
}

impl fmt::Display for PluginKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Playback => "playback",
            Self::Output => "output",
            Self::Resample => "resample",
        })
    }
}

/// A validated plugin, holding its library open for as long as it lives.
pub struct LoadedPlugin {
    descriptor: NonNull<c_void>,
    kind: PluginKind,
    name: String,
    version: String,
    library_version: String,
    path: PathBuf,
    // Dropped last: the descriptor points into this mapping.
    _library: Library,
}

impl LoadedPlugin {
    /// Returns the descriptor kind exported by the library.
    pub fn kind(&self) -> PluginKind {
        self.kind
    }

    /// Returns the plugin's advertised name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the plugin's advertised version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the advertised version of the underlying library.
    pub fn library_version(&self) -> &str {
        &self.library_version
    }

    /// Returns the shared-library path this plugin was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the playback descriptor when this is a playback plugin.
    pub fn playback(&self) -> Option<&RVPlaybackPlugin> {
        // SAFETY: the descriptor was published by the entry point of the library this
        // struct keeps loaded, `kind` records which type it was published as, and its
        // `api_version` was checked against the version those types mirror.
        (self.kind == PluginKind::Playback).then(|| unsafe { self.descriptor.cast().as_ref() })
    }

    /// Returns the output descriptor when this is an output plugin.
    pub fn output(&self) -> Option<&RVOutputPlugin> {
        // SAFETY: as `playback` above.
        (self.kind == PluginKind::Output).then(|| unsafe { self.descriptor.cast().as_ref() })
    }

    /// Returns the resample descriptor when this is a resample plugin.
    pub fn resample(&self) -> Option<&RVResamplePlugin> {
        // SAFETY: as `playback` above.
        (self.kind == PluginKind::Resample).then(|| unsafe { self.descriptor.cast().as_ref() })
    }
}

impl fmt::Debug for LoadedPlugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedPlugin")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("version", &self.version)
            .field("library_version", &self.library_version)
            .field("path", &self.path)
            .finish()
    }
}

/// Why one path yielded no plugin.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The path could not be inspected or enumerated.
    #[error("cannot read path: {0}")]
    Path(#[source] std::io::Error),
    /// The shared library could not be opened.
    #[error("dlopen failed: {0}")]
    Open(#[source] libloading::Error),
    /// The library exported no recognized plugin entry point.
    #[error("no plugin entry point")]
    NoEntryPoint,
    /// An entry point returned a null descriptor.
    #[error("{0} entry point returned a null descriptor")]
    NullDescriptor(PluginKind),
    /// The descriptor uses an unsupported ABI version.
    #[error("{kind} plugin has api_version {found}, expected {expected}")]
    ApiVersion {
        /// Kind of descriptor that failed validation.
        kind: PluginKind,
        /// ABI version found in the descriptor.
        found: u64,
        /// ABI version required by this crate.
        expected: u64,
    },
    /// The descriptor did not provide a non-empty name.
    #[error("{0} descriptor has no name")]
    MissingName(PluginKind),
    /// A descriptor string exceeded its corresponding byte limit.
    #[error("{kind} descriptor {field} exceeds its string limit")]
    DescriptorStringLimit {
        /// Kind of descriptor that failed validation.
        kind: PluginKind,
        /// Name of the overlong descriptor field.
        field: &'static str,
    },
    /// Routine rather than exceptional: a CMake `SOVERSION` install leaves
    /// `foo.so`, `foo.1.so` and `foo.1.0.0.so` side by side, all three loadable.
    #[error("{kind} plugin {name:?} is already loaded")]
    DuplicateName {
        /// Kind shared by both plugins.
        kind: PluginKind,
        /// Duplicate plugin name.
        name: String,
    },
}

/// One rejected path. The batch continues past it.
#[derive(Debug, thiserror::Error)]
#[error("{}: {source}", path.display())]
pub struct PluginError {
    /// Path that was rejected.
    pub path: PathBuf,
    /// Reason the path was rejected.
    #[source]
    pub source: LoadError,
}

/// The order plugins are probed in: pinned names first, pinned names last, everything
/// else alphabetical in between.
///
/// The default is the canon both existing hosts hardcode — `libopenmpt` recognizes
/// tracker formats most reliably, `uade` claims Amiga files broadly enough to shadow
/// the specialized players.
#[derive(Debug, Clone)]
pub struct ProbeOrder {
    /// Names pinned before unlisted plugins, in priority order.
    pub first: Vec<String>,
    /// Names pinned after unlisted plugins, in priority order.
    pub last: Vec<String>,
}

impl Default for ProbeOrder {
    fn default() -> Self {
        Self {
            first: vec!["libopenmpt".to_owned()],
            last: vec!["uade".to_owned()],
        }
    }
}

impl ProbeOrder {
    fn key<'a>(&self, name: &'a str) -> (u8, usize, &'a str) {
        if let Some(index) = self.first.iter().position(|pin| pin == name) {
            return (0, index, name);
        }
        if let Some(index) = self.last.iter().position(|pin| pin == name) {
            return (2, index, name);
        }
        (1, 0, name)
    }
}

/// Loaded plugins, ordered.
#[derive(Debug, Default)]
pub struct PluginSet {
    plugins: Vec<LoadedPlugin>,
}

impl PluginSet {
    /// Returns all loaded plugins in probe order.
    pub fn plugins(&self) -> &[LoadedPlugin] {
        &self.plugins
    }

    /// Iterates over plugins of `kind` in probe order.
    pub fn of_kind(&self, kind: PluginKind) -> impl Iterator<Item = &LoadedPlugin> {
        self.plugins
            .iter()
            .filter(move |plugin| plugin.kind == kind)
    }

    /// Mutably iterates over plugins of `kind` in probe order.
    pub fn of_kind_mut(&mut self, kind: PluginKind) -> impl Iterator<Item = &mut LoadedPlugin> {
        self.plugins
            .iter_mut()
            .filter(move |plugin| plugin.kind == kind)
    }

    /// Offers the file to each playback plugin in order: the first `Supported` wins,
    /// otherwise the first `Unsure`.
    ///
    /// `data` is the leading bytes of the file, `total_size` its full length.
    pub fn select_playback(
        &self,
        data: &[u8],
        filename: &str,
        total_size: u64,
    ) -> Option<&LoadedPlugin> {
        select_playback_candidate(
            self.of_kind(PluginKind::Playback),
            LoadedPlugin::playback,
            data,
            filename,
            total_size,
        )
    }
}

pub(crate) fn select_playback_candidate<'a, T: 'a>(
    candidates: impl IntoIterator<Item = &'a T>,
    descriptor: impl Fn(&'a T) -> Option<&'a RVPlaybackPlugin>,
    data: &[u8],
    filename: &str,
    total_size: u64,
) -> Option<&'a T> {
    if data.is_empty() {
        return None;
    }
    let filename = CString::new(filename).ok()?;

    let mut unsure = None;
    for candidate in candidates {
        let Some(probe) = descriptor(candidate).and_then(|plugin| plugin.probe_can_play) else {
            continue;
        };
        let mut probe_data = data.to_vec();
        // SAFETY: each plugin receives a private writable copy and a live C string.
        let raw = unsafe {
            probe(
                probe_data.as_mut_ptr(),
                probe_data.len() as u64,
                filename.as_ptr(),
                total_size,
            )
        };
        match RVProbeResult::from_raw(raw) {
            Some(RVProbeResult::Supported) => return Some(candidate),
            Some(RVProbeResult::Unsure) if unsure.is_none() => unsure = Some(candidate),
            _ => {}
        }
    }
    unsure
}

/// What one batch produced.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// Plugins that passed loading and descriptor validation.
    pub plugins: PluginSet,
    /// Paths rejected during the batch.
    pub errors: Vec<PluginError>,
}

/// Loads every plugin reachable from `paths`, in the default probe order.
pub fn load_plugins<I, P>(paths: I) -> LoadReport
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    load_plugins_ordered(paths, &ProbeOrder::default())
}

/// Loads every plugin reachable from `paths`, ordered by `order`.
///
/// Scanning, loading and validation all happen here; format probing waits for
/// [`PluginSet::select_playback`].
pub fn load_plugins_ordered<I, P>(paths: I, order: &ProbeOrder) -> LoadReport
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    let mut report = LoadReport::default();
    let mut seen = HashSet::new();

    for path in paths {
        let path = path.as_ref();
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_dir() => {
                for file in scan_directory(path, &mut report.errors) {
                    load_into(&file, &mut seen, &mut report);
                }
            }
            Ok(_) => load_into(path, &mut seen, &mut report),
            Err(error) => report.errors.push(PluginError {
                path: path.to_owned(),
                source: LoadError::Path(error),
            }),
        }
    }

    report
        .plugins
        .plugins
        .sort_by(|left, right| order.key(&left.name).cmp(&order.key(&right.name)));
    report
}

/// Candidate files directly inside `dir`, sorted so a batch is reproducible.
fn scan_directory(dir: &Path, errors: &mut Vec<PluginError>) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(PluginError {
                path: dir.to_owned(),
                source: LoadError::Path(error),
            });
            return Vec::new();
        }
    };

    let mut files = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                // uade ships a data directory beside its binary; a directory that
                // happens to match the canon is not a candidate.
                if path.is_file() && has_plugin_extension(&path) {
                    files.push(path);
                }
            }
            Err(error) => errors.push(PluginError {
                path: dir.to_owned(),
                source: LoadError::Path(error),
            }),
        }
    }
    files.sort();
    files
}

fn has_plugin_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            PLUGIN_EXTENSIONS
                .iter()
                .any(|canon| extension.eq_ignore_ascii_case(canon))
        })
}

fn load_into(path: &Path, seen: &mut HashSet<(PluginKind, String)>, report: &mut LoadReport) {
    match load_one(path, seen) {
        Ok(plugin) => report.plugins.plugins.push(plugin),
        Err(source) => report.errors.push(PluginError {
            path: path.to_owned(),
            source,
        }),
    }
}

/// The prefix every descriptor opens with, in every ABI version. Read before
/// `api_version` says whether the rest of the struct has the shape this version
/// declares — a v1 binary's descriptor is shorter than the v2 types mirror.
#[repr(C)]
struct DescriptorHeader {
    api_version: u64,
    name: *const c_char,
    version: *const c_char,
    library_version: *const c_char,
}

const _: () = {
    macro_rules! opens_with_the_header {
        ($ty:ty) => {
            assert!(offset_of!($ty, name) == offset_of!(DescriptorHeader, name));
            assert!(offset_of!($ty, version) == offset_of!(DescriptorHeader, version));
            assert!(
                offset_of!($ty, library_version) == offset_of!(DescriptorHeader, library_version)
            );
        };
    }
    opens_with_the_header!(RVPlaybackPlugin);
    opens_with_the_header!(RVOutputPlugin);
    opens_with_the_header!(RVResamplePlugin);
};

fn load_one(
    path: &Path,
    seen: &mut HashSet<(PluginKind, String)>,
) -> Result<LoadedPlugin, LoadError> {
    let library = open_library(path).map_err(LoadError::Open)?;

    let (kind, descriptor) = entry_point(&library).ok_or(LoadError::NoEntryPoint)?;
    let descriptor = NonNull::new(descriptor).ok_or(LoadError::NullDescriptor(kind))?;

    // SAFETY: an entry point returns a descriptor that opens with this header whatever
    // version it was built against; nothing past the header is touched yet.
    let header = unsafe { descriptor.cast::<DescriptorHeader>().as_ref() };

    if header.api_version != PLUGIN_API_VERSION {
        return Err(LoadError::ApiVersion {
            kind,
            found: header.api_version,
            expected: PLUGIN_API_VERSION,
        });
    }

    // SAFETY: the ABI declares these as C strings, and they stay readable for as long
    // as the library is loaded.
    let (name, version, library_version) = unsafe {
        (
            descriptor_string(header.name, PLUGIN_NAME_LIMIT),
            descriptor_string(header.version, PLUGIN_VERSION_LIMIT),
            descriptor_string(header.library_version, PLUGIN_LIBRARY_VERSION_LIMIT),
        )
    };

    let name = name.map_err(|_| LoadError::DescriptorStringLimit {
        kind,
        field: "name",
    })?;
    let version = version.map_err(|_| LoadError::DescriptorStringLimit {
        kind,
        field: "version",
    })?;
    let library_version = library_version.map_err(|_| LoadError::DescriptorStringLimit {
        kind,
        field: "library_version",
    })?;

    let name = name
        .filter(|name| !name.is_empty())
        .ok_or(LoadError::MissingName(kind))?;
    if !seen.insert((kind, name.clone())) {
        return Err(LoadError::DuplicateName { kind, name });
    }

    Ok(LoadedPlugin {
        descriptor,
        kind,
        name,
        version: version.unwrap_or_default(),
        library_version: library_version.unwrap_or_default(),
        path: path.to_owned(),
        _library: library,
    })
}

/// Binds eagerly, as the C loader does: a plugin with unresolved symbols has to fail
/// here rather than at its first call.
#[cfg(unix)]
fn open_library(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::unix::{Library as UnixLibrary, RTLD_LOCAL, RTLD_NOW};

    // SAFETY: loading runs the library's initializers; `path` is a host-nominated
    // plugin artifact, and the handle is owned by the `LoadedPlugin` built from it.
    unsafe { UnixLibrary::open(Some(path), RTLD_NOW | RTLD_LOCAL) }.map(Library::from)
}

#[cfg(not(unix))]
fn open_library(path: &Path) -> Result<Library, libloading::Error> {
    // SAFETY: as the unix arm above; `LoadLibrary` already binds eagerly.
    unsafe { Library::new(path) }
}

/// The first entry point the library resolves, and what it returned. A library
/// publishes one descriptor; the symbol that resolves is what says which kind.
fn entry_point(library: &Library) -> Option<(PluginKind, *mut c_void)> {
    for kind in PluginKind::ALL {
        // SAFETY: the symbol name and its signature are both fixed by the ABI, and
        // `library` outlives the borrow the symbol holds on it.
        let entry: Symbol<'_, unsafe extern "C" fn() -> *mut c_void> =
            match unsafe { library.get(kind.symbol()) } {
                Ok(entry) => entry,
                Err(_) => continue,
            };
        // SAFETY: the resolved symbol has the signature declared above.
        return Some((kind, unsafe { entry() }));
    }
    None
}

/// # Safety
///
/// `value` must satisfy [`plugin_str`]'s pointer contract for `limit`.
unsafe fn descriptor_string(
    value: *const c_char,
    limit: usize,
) -> Result<Option<String>, crate::plugin_string::StringLimitExceeded> {
    // SAFETY: the caller guarantees the bounded pointer contract.
    unsafe { plugin_str(value, limit) }.map(|value| value.map(|value| value.into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"not a shared library").expect("write");
        path
    }

    fn error_paths(report: &LoadReport) -> Vec<PathBuf> {
        let mut paths: Vec<_> = report
            .errors
            .iter()
            .map(|error| error.path.clone())
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn directories_are_scanned_for_the_extension_canon_on_every_platform() {
        let root = temp_dir();
        let mut expected = vec![
            touch(root.path(), "alpha.so"),
            touch(root.path(), "beta.dylib"),
            touch(root.path(), "gamma.dll"),
            touch(root.path(), "delta.rvp"),
            touch(root.path(), "epsilon.RVP"),
        ];
        expected.sort();
        touch(root.path(), "notes.txt");
        touch(root.path(), "extensionless");

        let report = load_plugins([root.path()]);

        assert!(report.plugins.plugins().is_empty());
        assert_eq!(error_paths(&report), expected);
    }

    #[test]
    fn directory_scans_reach_neither_subdirectories_nor_their_contents() {
        let root = temp_dir();
        let nested = root.path().join("data");
        std::fs::create_dir(&nested).expect("mkdir");
        touch(&nested, "buried.so");
        std::fs::create_dir(root.path().join("bundle.so")).expect("mkdir");

        let report = load_plugins([root.path()]);

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert!(report.plugins.plugins().is_empty());
    }

    #[test]
    fn files_named_directly_load_whatever_their_extension() {
        let root = temp_dir();
        let named = touch(root.path(), "plugin.bin");

        let report = load_plugins([&named]);

        assert_eq!(error_paths(&report), [named]);
    }

    #[test]
    fn an_unreadable_path_does_not_abort_the_batch() {
        let root = temp_dir();
        let missing = root.path().join("gone");
        let present = touch(root.path(), "present.so");

        let report = load_plugins([&missing, &present]);

        assert_eq!(report.errors.len(), 2);
        assert!(matches!(report.errors[0].source, LoadError::Path(_)));
        assert_eq!(report.errors[0].path, missing);
        assert!(matches!(report.errors[1].source, LoadError::Open(_)));
        assert_eq!(report.errors[1].path, present);

        for error in &report.errors {
            let reported = error.to_string();
            let cause = std::error::Error::source(error)
                .and_then(std::error::Error::source)
                .expect("the underlying error is the root cause");
            assert!(reported.contains(&cause.to_string()), "{reported}");
        }

        let path_cause = std::error::Error::source(&report.errors[0])
            .and_then(std::error::Error::source)
            .expect("the io error is the root cause");
        assert!(path_cause.is::<std::io::Error>());

        let open_cause = std::error::Error::source(&report.errors[1])
            .and_then(std::error::Error::source)
            .expect("the dlopen error is the root cause");
        assert!(open_cause.is::<libloading::Error>());

        let path_reported = report.errors[0].to_string();
        assert!(
            path_reported.contains(&missing.display().to_string()),
            "{path_reported}"
        );
    }

    #[test]
    fn the_dependency_graph_carries_neither_flowi_nor_anyhow() {
        let manifest = include_str!("../Cargo.toml");
        let lock = include_str!("../../Cargo.lock");
        for forbidden in ["anyhow", "flowi"] {
            assert!(!manifest.contains(forbidden), "{forbidden} in Cargo.toml");
            assert!(!lock.contains(forbidden), "{forbidden} in Cargo.lock");
        }
    }

    mod fixtures {
        use super::*;

        use crate::c_fixture::compile;

        /// A bare descriptor of `kind`: no probe, no callbacks.
        fn descriptor(dir: &Path, kind: PluginKind, stem: &str, api: u64, name: &str) -> PathBuf {
            let (header, ty, entry) = match kind {
                PluginKind::Playback => ("playback", "RVPlaybackPlugin", "rv_playback_plugin"),
                PluginKind::Output => ("output", "RVOutputPlugin", "rv_output_plugin"),
                PluginKind::Resample => ("resample", "RVResamplePlugin", "rv_resample_plugin"),
            };
            compile(
                dir,
                stem,
                &format!(
                    r#"#include <retrovert/{header}.h>
static {ty} plugin = {{
    .api_version = {api}, .name = "{name}", .version = "1.0", .library_version = "2.0",
}};
{ty}* {entry}(void) {{ return &plugin; }}
"#
                ),
            )
        }

        fn descriptor_fields(
            dir: &Path,
            stem: &str,
            name: &str,
            version: &str,
            library_version: &str,
        ) -> PathBuf {
            compile(
                dir,
                stem,
                &format!(
                    r#"#include <retrovert/playback.h>
static RVPlaybackPlugin plugin = {{
    .api_version = 2, .name = "{name}", .version = "{version}",
    .library_version = "{library_version}",
}};
RVPlaybackPlugin* rv_playback_plugin(void) {{ return &plugin; }}
"#
                ),
            )
        }

        /// A playback plugin whose probe runs `body` and counts its calls in a
        /// `probe_calls` symbol.
        fn probing(dir: &Path, name: &str, body: &str) -> PathBuf {
            compile(
                dir,
                name,
                &format!(
                    r#"#include <string.h>
#include <retrovert/playback.h>
static uint32_t calls;
uint32_t probe_calls(void) {{ return calls; }}
static RVProbeResult probe(uint8_t* data, uint64_t size, const char* file, uint64_t total) {{
    (void)data; (void)size; (void)file; (void)total;
    calls++;
    {body}
}}
static RVPlaybackPlugin plugin = {{
    .api_version = 2, .name = "{name}", .version = "1.0", .library_version = "2.0",
    .probe_can_play = probe,
}};
RVPlaybackPlugin* rv_playback_plugin(void) {{ return &plugin; }}
"#
                ),
            )
        }

        /// A playback plugin whose probe always answers `result`.
        fn answering(dir: &Path, name: &str, result: &str) -> PathBuf {
            probing(dir, name, &format!("return RVProbeResult_{result};"))
        }

        fn null_name(dir: &Path) -> PathBuf {
            compile(
                dir,
                "null_name",
                r#"#include <retrovert/playback.h>
static RVPlaybackPlugin plugin = { .api_version = 2 };
RVPlaybackPlugin* rv_playback_plugin(void) { return &plugin; }
"#,
            )
        }

        fn null_descriptor(dir: &Path) -> PathBuf {
            compile(
                dir,
                "null_descriptor",
                r#"#include <retrovert/playback.h>
RVPlaybackPlugin* rv_playback_plugin(void) { return 0; }
"#,
            )
        }

        fn no_entry_point(dir: &Path) -> PathBuf {
            compile(dir, "no_entry", "int unrelated(void) { return 0; }\n")
        }

        fn unload_notifier(dir: &Path, event_path: &Path) -> PathBuf {
            let event_path = event_path
                .display()
                .to_string()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            compile(
                dir,
                "unload_notifier",
                &format!(
                    r#"#include <stdio.h>
#include <retrovert/playback.h>

#ifndef _WIN32
static void note_unload(void) {{
    FILE* file = fopen("{event_path}", "w");
    if (file != NULL) {{ fputs("unloaded", file); fclose(file); }}
}}
#endif

static RVProbeResult probe(uint8_t* data, uint64_t size, const char* name, uint64_t total) {{
    (void)data; (void)size; (void)total;
    return name[0] == 'w' ? RVProbeResult_Supported : RVProbeResult_Unsupported;
}}

static RVPlaybackPlugin plugin = {{
    .api_version = 2, .name = "windows", .version = "1.0", .library_version = "2.0",
    .probe_can_play = probe,
}};
RVPlaybackPlugin* rv_playback_plugin(void) {{ return &plugin; }}

#ifndef _WIN32
__attribute__((destructor)) static void library_unload(void) {{ note_unload(); }}
#endif
"#
                ),
            )
        }

        /// A descriptor a lazy bind would accept, in a library calling a symbol that
        /// nothing defines.
        #[cfg(target_os = "linux")]
        fn unresolved_symbol(dir: &Path) -> PathBuf {
            compile(
                dir,
                "unresolved",
                r#"#include <retrovert/playback.h>
extern int missing_thing(void);
int call_it(void) { return missing_thing(); }
static RVPlaybackPlugin plugin = {
    .api_version = 2, .name = "unresolved", .version = "1.0", .library_version = "2.0",
};
RVPlaybackPlugin* rv_playback_plugin(void) { return &plugin; }
"#,
            )
        }

        /// The fixture's probe counter, read through a second handle on the same mapping.
        fn probe_calls(path: &Path) -> u32 {
            // SAFETY: the fixture is already loaded, so this resolves to the same
            // mapping, and its `probe_calls` has the signature named here.
            unsafe {
                let library = Library::new(path).expect("reopen fixture");
                let calls: Symbol<'_, unsafe extern "C" fn() -> u32> =
                    library.get(b"probe_calls\0").expect("probe_calls");
                calls()
            }
        }

        fn names(set: &PluginSet) -> Vec<&str> {
            set.plugins().iter().map(LoadedPlugin::name).collect()
        }

        fn selected<'a>(report: &'a LoadReport, filename: &str) -> Option<&'a str> {
            report
                .plugins
                .select_playback(b"song", filename, 4)
                .map(LoadedPlugin::name)
        }

        #[test]
        fn all_three_plugin_kinds_load() {
            let root = temp_dir();
            descriptor(root.path(), PluginKind::Playback, "player", 2, "player");
            descriptor(root.path(), PluginKind::Output, "speaker", 2, "speaker");
            descriptor(
                root.path(),
                PluginKind::Resample,
                "converter",
                2,
                "converter",
            );

            let report = load_plugins([root.path()]);

            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(names(&report.plugins), ["converter", "player", "speaker"]);

            let by_kind = |kind| {
                report
                    .plugins
                    .of_kind(kind)
                    .map(LoadedPlugin::name)
                    .collect::<Vec<_>>()
            };
            assert_eq!(by_kind(PluginKind::Playback), ["player"]);
            assert_eq!(by_kind(PluginKind::Output), ["speaker"]);
            assert_eq!(by_kind(PluginKind::Resample), ["converter"]);

            let plugins = report.plugins.plugins();
            assert!(plugins[0].resample().is_some());
            assert!(plugins[1].playback().is_some());
            assert!(plugins[1].output().is_none());
            assert!(plugins[2].output().is_some());
            assert_eq!(plugins[1].version(), "1.0");
            assert_eq!(plugins[1].library_version(), "2.0");
            assert_eq!(
                plugins[1]
                    .path()
                    .extension()
                    .and_then(|value| value.to_str()),
                Some(std::env::consts::DLL_EXTENSION)
            );
        }

        #[test]
        fn a_native_library_loads_selects_and_unloads() {
            let root = temp_dir();
            let event_path = root.path().join("unload-event");
            let fixture = unload_notifier(root.path(), &event_path);

            let report = load_plugins([&fixture]);

            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(names(&report.plugins), ["windows"]);
            assert_eq!(selected(&report, "windows.mod"), Some("windows"));
            assert_eq!(selected(&report, "other.mod"), None);

            drop(report);

            #[cfg(windows)]
            std::fs::remove_file(fixture).expect("unloaded library can be removed");
            #[cfg(not(windows))]
            assert_eq!(
                std::fs::read_to_string(event_path).expect("library unload event"),
                "unloaded"
            );
        }

        #[test]
        fn v1_binaries_are_rejected_whatever_kind_they_claim() {
            let root = temp_dir();
            let stale = [
                descriptor(root.path(), PluginKind::Playback, "old_player", 1, "player"),
                descriptor(root.path(), PluginKind::Output, "old_speaker", 1, "speaker"),
                descriptor(
                    root.path(),
                    PluginKind::Resample,
                    "old_converter",
                    1,
                    "converter",
                ),
            ];

            let report = load_plugins(&stale);

            assert!(report.plugins.plugins().is_empty());
            assert_eq!(report.errors.len(), 3);
            for error in &report.errors {
                assert!(
                    matches!(
                        error.source,
                        LoadError::ApiVersion {
                            found: 1,
                            expected: 2,
                            ..
                        }
                    ),
                    "{error}"
                );
            }
        }

        /// macOS needs `-Wl,-undefined,dynamic_lookup` for the fixture and its dyld
        /// behaviour is unverified here, so this stays Linux-only.
        #[cfg(target_os = "linux")]
        #[test]
        fn a_library_with_unresolved_symbols_fails_at_load_not_at_first_call() {
            let root = temp_dir();
            let broken = unresolved_symbol(root.path());

            let report = load_plugins([&broken]);

            assert!(report.plugins.plugins().is_empty());
            assert_eq!(report.errors.len(), 1);
            assert!(
                matches!(report.errors[0].source, LoadError::Open(_)),
                "{}",
                report.errors[0]
            );
        }

        #[test]
        fn validation_rejects_null_descriptors_missing_entry_points_and_unnamed_plugins() {
            let root = temp_dir();
            let null = null_descriptor(root.path());
            let anonymous = null_name(root.path());
            let blank = descriptor(root.path(), PluginKind::Playback, "blank", 2, "");
            let missing_entry = no_entry_point(root.path());
            let good = descriptor(root.path(), PluginKind::Playback, "good", 2, "good");

            let report = load_plugins([&null, &anonymous, &blank, &missing_entry, &good]);

            assert_eq!(names(&report.plugins), ["good"]);
            assert!(matches!(
                report.errors[0].source,
                LoadError::NullDescriptor(PluginKind::Playback)
            ));
            assert!(matches!(
                report.errors[1].source,
                LoadError::MissingName(PluginKind::Playback)
            ));
            assert!(matches!(
                report.errors[2].source,
                LoadError::MissingName(PluginKind::Playback)
            ));
            assert!(matches!(report.errors[3].source, LoadError::NoEntryPoint));
            assert_eq!(report.errors.len(), 4);
        }

        #[test]
        fn descriptor_strings_accept_their_limits_and_report_each_over_limit_field() {
            let root = temp_dir();
            let exact = descriptor_fields(
                root.path(),
                "exact",
                &"n".repeat(PLUGIN_NAME_LIMIT),
                &"v".repeat(PLUGIN_VERSION_LIMIT),
                &"l".repeat(PLUGIN_LIBRARY_VERSION_LIMIT),
            );
            let report = load_plugins([exact]);
            assert_eq!(report.plugins.plugins().len(), 1);
            assert!(report.errors.is_empty());

            for (stem, name, version, library_version, field) in [
                (
                    "long_name",
                    "n".repeat(PLUGIN_NAME_LIMIT + 1),
                    "v".repeat(PLUGIN_VERSION_LIMIT),
                    "l".repeat(PLUGIN_LIBRARY_VERSION_LIMIT),
                    "name",
                ),
                (
                    "long_version",
                    "n".repeat(PLUGIN_NAME_LIMIT),
                    "v".repeat(PLUGIN_VERSION_LIMIT + 1),
                    "l".repeat(PLUGIN_LIBRARY_VERSION_LIMIT),
                    "version",
                ),
                (
                    "long_library_version",
                    "n".repeat(PLUGIN_NAME_LIMIT),
                    "v".repeat(PLUGIN_VERSION_LIMIT),
                    "l".repeat(PLUGIN_LIBRARY_VERSION_LIMIT + 1),
                    "library_version",
                ),
            ] {
                let path = descriptor_fields(root.path(), stem, &name, &version, &library_version);
                let report = load_plugins([path]);
                assert!(matches!(
                    report.errors[0].source,
                    LoadError::DescriptorStringLimit {
                        kind: PluginKind::Playback,
                        field: found,
                    } if found == field
                ));
            }
        }

        #[test]
        fn a_repeated_name_keeps_the_first_file_scanned_and_reports_the_rest() {
            let root = temp_dir();
            let first = descriptor(root.path(), PluginKind::Playback, "twin", 2, "twin");
            let versioned = root
                .path()
                .join(format!("twin.1.0.0.{}", std::env::consts::DLL_EXTENSION));
            std::fs::copy(&first, &versioned).expect("copy fixture");

            let report = load_plugins([root.path()]);

            assert_eq!(names(&report.plugins), ["twin"]);
            assert_eq!(report.plugins.plugins()[0].path(), versioned);
            assert_eq!(report.errors.len(), 1);
            assert_eq!(report.errors[0].path, first);
            assert!(matches!(
                report.errors[0].source,
                LoadError::DuplicateName {
                    kind: PluginKind::Playback,
                    ..
                }
            ));
        }

        #[test]
        fn plugins_of_different_kinds_may_share_a_name() {
            let root = temp_dir();
            descriptor(root.path(), PluginKind::Playback, "a_shared", 2, "shared");
            descriptor(root.path(), PluginKind::Output, "b_shared", 2, "shared");

            let report = load_plugins([root.path()]);

            assert!(report.errors.is_empty(), "{:?}", report.errors);
            assert_eq!(names(&report.plugins), ["shared", "shared"]);
        }

        #[test]
        fn probe_order_puts_libopenmpt_first_and_uade_last() {
            let root = temp_dir();
            for name in ["zeta", "uade", "alpha", "libopenmpt"] {
                descriptor(root.path(), PluginKind::Playback, name, 2, name);
            }

            let report = load_plugins([root.path()]);

            assert_eq!(
                names(&report.plugins),
                ["libopenmpt", "alpha", "zeta", "uade"]
            );
        }

        #[test]
        fn a_host_can_pin_its_own_order() {
            let root = temp_dir();
            for name in ["zeta", "uade", "alpha", "libopenmpt"] {
                descriptor(root.path(), PluginKind::Playback, name, 2, name);
            }
            let order = ProbeOrder {
                first: vec!["zeta".to_owned(), "uade".to_owned()],
                last: vec!["libopenmpt".to_owned()],
            };

            let report = load_plugins_ordered([root.path()], &order);

            assert_eq!(
                names(&report.plugins),
                ["zeta", "uade", "alpha", "libopenmpt"]
            );
        }

        #[test]
        fn selection_takes_the_first_supported_then_the_first_unsure() {
            let root = temp_dir();
            answering(root.path(), "a_declines", "Unsupported");
            answering(root.path(), "b_guesses", "Unsure");
            answering(root.path(), "c_guesses_too", "Unsure");
            let claiming = answering(root.path(), "d_claims", "Supported");

            // A later definite claim beats an earlier guess.
            assert_eq!(
                selected(&load_plugins([root.path()]), "song.mod"),
                Some("d_claims")
            );

            std::fs::remove_file(&claiming).expect("remove fixture");
            assert_eq!(
                selected(&load_plugins([root.path()]), "song.mod"),
                Some("b_guesses")
            );
        }

        #[test]
        fn nothing_claims_the_file_when_every_plugin_declines() {
            let root = temp_dir();
            answering(root.path(), "declines", "Unsupported");

            let report = load_plugins([root.path()]);

            assert_eq!(selected(&report, "song.mod"), None);
        }

        #[test]
        fn a_plugin_without_a_probe_never_claims_a_file() {
            let root = temp_dir();
            descriptor(root.path(), PluginKind::Playback, "a_silent", 2, "a_silent");
            answering(root.path(), "b_guesses", "Unsure");

            let report = load_plugins([root.path()]);

            assert_eq!(names(&report.plugins), ["a_silent", "b_guesses"]);
            assert_eq!(selected(&report, "song.mod"), Some("b_guesses"));
        }

        #[test]
        fn each_plugin_probes_an_unscribbled_copy() {
            let root = temp_dir();
            probing(
                root.path(),
                "a_scribbles",
                "data[0] = 'X'; return RVProbeResult_Unsupported;",
            );
            probing(
                root.path(),
                "b_reads",
                "return data[0] == 's' ? RVProbeResult_Supported : RVProbeResult_Unsupported;",
            );

            let report = load_plugins([root.path()]);

            assert_eq!(selected(&report, "song.mod"), Some("b_reads"));
        }

        #[test]
        fn the_probe_receives_the_bytes_and_the_name_its_caller_passed() {
            let root = temp_dir();
            probing(
                root.path(),
                "strict",
                r#"return (size == 4 && total == 12345 && data[0] == 's'
                          && strcmp(file, "song.mod") == 0)
                       ? RVProbeResult_Supported : RVProbeResult_Unsupported;"#,
            );

            let report = load_plugins([root.path()]);

            assert_eq!(
                report
                    .plugins
                    .select_playback(b"song", "song.mod", 12345)
                    .map(LoadedPlugin::name),
                Some("strict")
            );
            assert_eq!(selected(&report, "other.mod"), None);
        }

        #[test]
        fn format_probing_waits_until_selection() {
            let root = temp_dir();
            let fixture = answering(root.path(), "counter", "Supported");

            let report = load_plugins([&fixture]);
            assert_eq!(probe_calls(&fixture), 0);

            // Neither an empty buffer nor an unrepresentable filename reaches a plugin.
            assert!(report.plugins.select_playback(b"", "song.mod", 0).is_none());
            assert!(report
                .plugins
                .select_playback(b"song", "song\0.mod", 4)
                .is_none());
            assert_eq!(probe_calls(&fixture), 0);

            selected(&report, "song.mod");
            assert_eq!(probe_calls(&fixture), 1);
        }
    }
}
