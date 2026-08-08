//! The services a host lends to plugins, and the C vtables that carry them.
//!
//! One trait per service, never a bundle: a host that only wants its own logging swaps
//! [`Log`] and keeps the crate's file access. Every vtable slot the plugin can call is
//! wrapped so a panic in host code becomes a failed call instead of a torn-down process.

mod io;
mod log;
mod settings;

pub use io::{Io, StdFsIo};
pub use log::{retrovert_host_log_write, Log, LogCrate, LogLevel};
pub use settings::{
    Choice, MemorySettingsStore, Setting, SettingSchema, SettingsError, SettingsHandle,
    SettingsStore, SettingsUpdate, StoredValue, Value,
};

use core::ffi::{c_char, c_int, c_void, CStr};
use core::panic::AssertUnwindSafe;
use core::ptr;
use std::borrow::Cow;
use std::panic::catch_unwind;
use std::sync::Arc;

use crate::ffi::io::{RVIo, RV_IO_API_VERSION};
use crate::ffi::log::{RVLog, RVLogFn, RV_LOG_API_VERSION};
use crate::ffi::metadata::RVMetadata;
use crate::ffi::service::RVService;
use crate::ffi::settings::{RVSettings, RV_SETTINGS_API_VERSION};

/// Runs a vtable callback, turning a panic into `fallback`; unwinding into C is undefined.
fn guard<R>(fallback: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or(fallback)
}

/// Decodes a string a plugin passed us. Invalid UTF-8 is replaced rather than rejected.
///
/// # Safety
///
/// `value` must be null, or a NUL-terminated string readable for its whole length. The
/// returned borrow must not outlive that string.
unsafe fn plugin_str<'a>(value: *const c_char) -> Option<Cow<'a, str>> {
    if value.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a readable NUL-terminated string when non-null.
    Some(unsafe { CStr::from_ptr(value) }.to_string_lossy())
}

/// # Safety
///
/// `private_data` must be the context pointer [`ServiceHost::new`] installed in every
/// vtable, and the context must still be alive.
unsafe fn context<'a>(private_data: *mut c_void) -> &'a ServiceContext {
    // SAFETY: the caller guarantees a live context pointer, and callbacks only ever share it.
    unsafe { &*private_data.cast::<ServiceContext>() }
}

/// The vtables handed to plugins, kept beside the host implementations they dispatch to.
///
/// Each vtable's `private_data` points back here, so a callback recovers the whole set from
/// the one pointer C gives it.
struct ServiceContext {
    io_vtable: RVIo,
    log_vtable: RVLog,
    settings_vtable: RVSettings,
    service: RVService,
    io: Box<dyn Io>,
    log: Arc<dyn Log>,
    settings: SettingsHandle,
}

/// A live set of services, ready to be handed to plugins.
///
/// The [`Default`] set reads local files, logs through the `log` crate, and keeps settings
/// in memory. Dropping the host invalidates the vtables, so it must outlive every plugin
/// that was given them.
pub struct ServiceHost {
    ctx: *mut ServiceContext,
    settings: SettingsHandle,
}

// SAFETY: the context is only reachable through `ServiceHost`, which owns it exclusively,
// and everything it holds is `Send`.
unsafe impl Send for ServiceHost {}

impl Default for ServiceHost {
    fn default() -> Self {
        Self::new(
            Box::new(StdFsIo),
            Arc::new(LogCrate),
            Box::new(MemorySettingsStore::default()),
        )
    }
}

impl ServiceHost {
    pub fn new(io: Box<dyn Io>, log: Arc<dyn Log>, store: Box<dyn SettingsStore>) -> Self {
        let settings = SettingsHandle::new(store, Arc::clone(&log));
        let ctx = Box::into_raw(Box::new(ServiceContext {
            io_vtable: RVIo {
                private_data: ptr::null_mut(),
                exists: Some(io::exists),
                read_url_to_memory: Some(io::read_url_to_memory),
                free_url_to_memory: Some(io::free_url_to_memory),
            },
            log_vtable: RVLog {
                private_data: ptr::null_mut(),
                log: Some(log::retrovert_host_log_shim as RVLogFn),
            },
            settings_vtable: RVSettings {
                private_data: ptr::null_mut(),
                reg: Some(settings::reg),
                get_string: Some(settings::get_string),
                get_int: Some(settings::get_int),
                get_float: Some(settings::get_float),
                get_bool: Some(settings::get_bool),
            },
            service: RVService {
                private_data: ptr::null_mut(),
                get_io: Some(get_io),
                get_log: Some(get_log),
                get_metadata: Some(get_metadata),
                get_settings: Some(get_settings),
            },
            io,
            log,
            settings: settings.clone(),
        }));

        // SAFETY: `ctx` is a fresh, uniquely owned allocation; these writes only fill in the
        // back-pointers the callbacks recover the context from.
        unsafe {
            (*ctx).io_vtable.private_data = ctx.cast();
            (*ctx).log_vtable.private_data = ctx.cast();
            (*ctx).settings_vtable.private_data = ctx.cast();
            (*ctx).service.private_data = ctx.cast();
        }

        Self { ctx, settings }
    }

    /// Enumerate, read and write plugin settings. Cloneable and shared with the plugins.
    pub fn settings(&self) -> SettingsHandle {
        self.settings.clone()
    }

    /// The locator every plugin entry point takes.
    pub fn service(&self) -> &RVService {
        // SAFETY: `self.ctx` is live for as long as `self` is, and is only ever shared.
        unsafe { &(*self.ctx).service }
    }
}

impl Drop for ServiceHost {
    fn drop(&mut self) {
        // SAFETY: `self.ctx` came from `Box::into_raw` in `new` and is dropped once.
        drop(unsafe { Box::from_raw(self.ctx) });
    }
}

/// # Safety
///
/// `private_data` must be the context pointer installed by [`ServiceHost::new`].
unsafe extern "C" fn get_io(private_data: *mut c_void, api_version: c_int) -> *const RVIo {
    guard(ptr::null(), || {
        if api_version != RV_IO_API_VERSION {
            return ptr::null();
        }
        // SAFETY: the caller guarantees the context pointer.
        &raw const unsafe { context(private_data) }.io_vtable
    })
}

/// # Safety
///
/// `private_data` must be the context pointer installed by [`ServiceHost::new`].
unsafe extern "C" fn get_log(private_data: *mut c_void, api_version: c_int) -> *const RVLog {
    guard(ptr::null(), || {
        if api_version != RV_LOG_API_VERSION {
            return ptr::null();
        }
        // SAFETY: the caller guarantees the context pointer.
        &raw const unsafe { context(private_data) }.log_vtable
    })
}

/// The metadata service has no host trait and no implementation yet, so a plugin that asks
/// for one is told there is none.
///
/// # Safety
///
/// `private_data` is unused; any pointer value is accepted.
unsafe extern "C" fn get_metadata(
    _private_data: *mut c_void,
    _api_version: c_int,
) -> *const RVMetadata {
    guard(ptr::null(), ptr::null)
}

/// # Safety
///
/// `private_data` must be the context pointer installed by [`ServiceHost::new`].
unsafe extern "C" fn get_settings(
    private_data: *mut c_void,
    api_version: c_int,
) -> *const RVSettings {
    guard(ptr::null(), || {
        if api_version != RV_SETTINGS_API_VERSION {
            return ptr::null();
        }
        // SAFETY: the caller guarantees the context pointer.
        &raw const unsafe { context(private_data) }.settings_vtable
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::ffi::CStr;
    use std::ffi::CString;
    use std::sync::Mutex;

    use crate::ffi::log::RVLogLevel;
    use crate::ffi::settings::{
        RVSBool, RVSFloat, RVSInteger, RVSIntegerFixedRange, RVSIntegerRangeValue,
        RVSStringFixedRange, RVSStringRangeValue, RVSStringResult, RVSetting, RVSettingsResult,
        RVS_BOOL_TYPE, RVS_FLOAT_TYPE, RVS_INTEGER_RANGE_TYPE, RVS_INTEGER_TYPE,
        RVS_STRING_RANGE_TYPE,
    };
    use crate::service::settings::Choice;

    #[derive(Default)]
    pub(super) struct CapturingLog {
        pub(super) lines: Mutex<Vec<String>>,
    }

    impl Log for CapturingLog {
        fn log(&self, level: LogLevel, file: Option<&str>, line: u32, message: &str) {
            let location = file
                .map(|file| format!("{file}:{line} "))
                .unwrap_or_default();
            self.lines
                .lock()
                .expect("log lines")
                .push(format!("{level:?} {location}{message}"));
        }
    }

    struct PanickingIo;

    impl Io for PanickingIo {
        fn exists(&self, _url: &str) -> bool {
            panic!("host io blew up")
        }

        fn read_to_memory(&self, _url: &str) -> Option<Vec<u8>> {
            panic!("host io blew up")
        }
    }

    struct PanickingLog;

    impl Log for PanickingLog {
        fn log(&self, _level: LogLevel, _file: Option<&str>, _line: u32, _message: &str) {
            panic!("host log blew up")
        }
    }

    struct PanickingStore;

    impl SettingsStore for PanickingStore {
        fn load(&self, _reg_id: &str) -> Vec<StoredValue> {
            panic!("host store blew up")
        }

        fn save(&self, _reg_id: &str, _values: &[StoredValue]) {
            panic!("host store blew up")
        }
    }

    pub(super) fn with_log(log: Arc<dyn Log>) -> ServiceHost {
        ServiceHost::new(
            Box::new(StdFsIo),
            log,
            Box::new(MemorySettingsStore::default()),
        )
    }

    fn io_vtable(host: &ServiceHost) -> &RVIo {
        let service = host.service();
        // SAFETY: the getter answers the version it was built for with a live vtable.
        unsafe { &*(service.get_io.expect("get_io"))(service.private_data, RV_IO_API_VERSION) }
    }

    fn settings_vtable(host: &ServiceHost) -> &RVSettings {
        let service = host.service();
        // SAFETY: the getter answers the version it was built for with a live vtable.
        unsafe {
            &*(service.get_settings.expect("get_settings"))(
                service.private_data,
                RV_SETTINGS_API_VERSION,
            )
        }
    }

    /// The whole array `openmpt` registers in miniature: one setting of each widget kind.
    fn every_widget_kind(
        int_choices: &mut [RVSIntegerRangeValue],
        str_choices: &mut [RVSStringRangeValue],
    ) -> [RVSetting; 5] {
        [
            RVSetting {
                float_value: RVSFloat {
                    widget_id: c"gain".as_ptr(),
                    name: c"Master Gain".as_ptr(),
                    desc: c"in percent".as_ptr(),
                    widget_type: RVS_FLOAT_TYPE,
                    value: 100.0,
                    start_range: 0.0,
                    end_range: 400.0,
                },
            },
            RVSetting {
                int_value: RVSInteger {
                    widget_id: c"separation".as_ptr(),
                    name: c"Stereo Separation".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: RVS_INTEGER_TYPE,
                    value: 100,
                    start_range: 0,
                    end_range: 200,
                },
            },
            RVSetting {
                bool_value: RVSBool {
                    widget_id: c"amiga".as_ptr(),
                    name: c"Amiga Resampling".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: RVS_BOOL_TYPE,
                    value: true,
                },
            },
            RVSetting {
                int_fixed_value: RVSIntegerFixedRange {
                    widget_id: c"channels".as_ptr(),
                    name: c"Channels".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: RVS_INTEGER_RANGE_TYPE,
                    value: 2,
                    values: int_choices.as_mut_ptr(),
                    values_size: int_choices.len() as u64,
                },
            },
            RVSetting {
                string_fixed_value: RVSStringFixedRange {
                    widget_id: c"filter".as_ptr(),
                    name: c"Amiga Resampler Filter".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: RVS_STRING_RANGE_TYPE,
                    value: c"auto".as_ptr(),
                    values: str_choices.as_mut_ptr(),
                    values_size: str_choices.len() as u64,
                },
            },
        ]
    }

    fn register(host: &ServiceHost, reg_id: &CStr, settings: &mut [RVSetting]) -> u32 {
        let vtable = settings_vtable(host);
        // SAFETY: the array outlives the call and every pointer in it is readable.
        unsafe {
            (vtable.reg.expect("reg"))(
                vtable.private_data,
                reg_id.as_ptr(),
                settings.as_mut_ptr(),
                settings.len() as u64,
            )
        }
    }

    #[test]
    fn a_host_can_be_built_on_one_thread_and_handed_to_another() {
        fn assert_send<T: Send>() {}
        assert_send::<ServiceHost>();
        assert_send::<SettingsHandle>();
    }

    #[test]
    fn each_service_answers_its_own_version_and_nothing_else() {
        let host = ServiceHost::default();
        let service = host.service();

        // SAFETY: the locator is live and the getters take any version.
        unsafe {
            assert!(
                !(service.get_io.expect("get_io"))(service.private_data, RV_IO_API_VERSION)
                    .is_null()
            );
            assert!((service.get_io.expect("get_io"))(service.private_data, 2).is_null());

            assert!(
                !(service.get_log.expect("get_log"))(service.private_data, RV_LOG_API_VERSION)
                    .is_null()
            );
            assert!((service.get_log.expect("get_log"))(service.private_data, 2).is_null());

            assert!(!(service.get_settings.expect("get_settings"))(
                service.private_data,
                RV_SETTINGS_API_VERSION
            )
            .is_null());
            assert!(
                (service.get_settings.expect("get_settings"))(service.private_data, 2).is_null()
            );

            // No metadata service exists yet, so a plugin asking for one is told so.
            assert!(
                (service.get_metadata.expect("get_metadata"))(service.private_data, 1).is_null()
            );
        }
    }

    #[test]
    fn a_file_read_through_the_vtable_comes_back_whole_and_is_freed_through_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("song.mod");
        std::fs::write(&path, b"payload").expect("write");
        let url = CString::new(path.to_str().expect("utf-8 path")).expect("url");

        let host = ServiceHost::default();
        let vtable = io_vtable(&host);

        // SAFETY: `url` is a live NUL-terminated string and the result is freed once.
        unsafe {
            assert!((vtable.exists.expect("exists"))(
                vtable.private_data,
                url.as_ptr()
            ));

            let read =
                (vtable.read_url_to_memory.expect("read"))(vtable.private_data, url.as_ptr());
            assert_eq!(read.data_size, 7);
            assert_eq!(
                core::slice::from_raw_parts(read.data, read.data_size as usize),
                b"payload"
            );
            (vtable.free_url_to_memory.expect("free"))(vtable.private_data, read.data.cast());
        }
    }

    #[test]
    fn a_url_that_cannot_be_read_comes_back_empty_rather_than_failing() {
        let host = ServiceHost::default();
        let vtable = io_vtable(&host);
        let missing = CString::new("/definitely/not/here.mod").expect("url");

        // SAFETY: `missing` is a live NUL-terminated string; null is explicitly allowed.
        unsafe {
            assert!(!(vtable.exists.expect("exists"))(
                vtable.private_data,
                missing.as_ptr()
            ));
            assert!(!(vtable.exists.expect("exists"))(
                vtable.private_data,
                ptr::null()
            ));

            let read =
                (vtable.read_url_to_memory.expect("read"))(vtable.private_data, missing.as_ptr());
            assert!(read.data.is_null());
            assert_eq!(read.data_size, 0);

            // Freeing nothing is what a plugin does after an empty read.
            (vtable.free_url_to_memory.expect("free"))(vtable.private_data, ptr::null_mut());
        }
    }

    #[test]
    fn the_log_shim_formats_the_plugins_arguments_and_keeps_level_and_location() {
        let captured = Arc::new(CapturingLog::default());
        let host = with_log(captured.clone());
        let service = host.service();

        // SAFETY: the getter is live, and both calls pass readable format strings.
        unsafe {
            let vtable =
                &*(service.get_log.expect("get_log"))(service.private_data, RV_LOG_API_VERSION);
            let log = vtable.log.expect("log");
            log(
                vtable.private_data,
                RVLogLevel::Warn as u32,
                ptr::null(),
                0,
                c"cannot open %s (%d)".as_ptr(),
                c"song.mod".as_ptr(),
                7,
            );
            log(
                vtable.private_data,
                RVLogLevel::Trace as u32,
                c"uade.c".as_ptr(),
                42,
                c"ready".as_ptr(),
            );
            // A level this ABI version does not define is not a reason to lose the line.
            log(
                vtable.private_data,
                99,
                ptr::null(),
                0,
                c"from the future".as_ptr(),
            );
        }

        let lines = captured.lines.lock().expect("log lines");
        assert_eq!(
            *lines,
            [
                "Warn cannot open song.mod (7)",
                "Trace uade.c:42 ready",
                "Info from the future",
            ]
        );
    }

    #[test]
    fn a_plugin_registers_every_widget_kind_and_the_host_sees_the_whole_schema() {
        let mut int_choices = [
            RVSIntegerRangeValue {
                name: c"Mono".as_ptr(),
                value: 1,
            },
            RVSIntegerRangeValue {
                name: c"Stereo".as_ptr(),
                value: 2,
            },
        ];
        let mut str_choices = [RVSStringRangeValue {
            name: c"A500 Filter".as_ptr(),
            value: c"a500".as_ptr(),
        }];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);

        let host = ServiceHost::default();
        assert_eq!(
            register(&host, c"libopenmpt", &mut settings),
            RVSettingsResult::Ok as u32
        );

        let schemas = host.settings().schemas("libopenmpt");
        assert_eq!(
            schemas.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["gain", "separation", "amiga", "channels", "filter"]
        );
        assert_eq!(schemas[0].name, "Master Gain");
        assert_eq!(schemas[0].desc, "in percent");
        assert_eq!(
            schemas[0].setting,
            Setting::Float {
                value: 100.0,
                start_range: 0.0,
                end_range: 400.0
            }
        );
        assert_eq!(
            schemas[1].setting,
            Setting::Int {
                value: 100,
                start_range: 0,
                end_range: 200
            }
        );
        assert_eq!(schemas[2].setting, Setting::Bool { value: true });
        assert_eq!(
            schemas[3].setting,
            Setting::IntChoice {
                value: 2,
                choices: vec![
                    Choice {
                        name: "Mono".to_string(),
                        value: 1
                    },
                    Choice {
                        name: "Stereo".to_string(),
                        value: 2
                    },
                ]
            }
        );
        assert_eq!(
            schemas[4].setting,
            Setting::StrChoice {
                value: "auto".to_string(),
                choices: vec![Choice {
                    name: "A500 Filter".to_string(),
                    value: "a500".to_string()
                }]
            }
        );
    }

    #[test]
    fn a_setting_without_an_id_or_with_an_unknown_widget_type_is_reported_and_skipped() {
        let captured = Arc::new(CapturingLog::default());
        let host = with_log(captured.clone());
        let mut settings = [
            RVSetting {
                int_value: RVSInteger {
                    widget_id: ptr::null(),
                    name: c"Nameless".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: RVS_INTEGER_TYPE,
                    value: 1,
                    start_range: 0,
                    end_range: 0,
                },
            },
            RVSetting {
                int_value: RVSInteger {
                    widget_id: c"strange".as_ptr(),
                    name: c"Strange".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: 0x2000,
                    value: 1,
                    start_range: 0,
                    end_range: 0,
                },
            },
            RVSetting {
                int_value: RVSInteger {
                    widget_id: c"good".as_ptr(),
                    name: c"Good".as_ptr(),
                    desc: c"".as_ptr(),
                    widget_type: RVS_INTEGER_TYPE,
                    value: 3,
                    start_range: 0,
                    end_range: 0,
                },
            },
        ];

        assert_eq!(
            register(&host, c"uade", &mut settings),
            RVSettingsResult::Ok as u32
        );

        let schemas = host.settings().schemas("uade");
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].id, "good");
        let lines = captured.lines.lock().expect("log lines");
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("without an id"), "{lines:?}");
        assert!(lines[1].contains("unknown widget type 0x2000"), "{lines:?}");
    }

    #[test]
    fn the_abi_reports_a_duplicate_a_missing_module_an_unknown_id_and_a_wrong_type_apart() {
        let mut int_choices = [];
        let mut str_choices = [];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);
        let host = ServiceHost::default();
        register(&host, c"libopenmpt", &mut settings);

        assert_eq!(
            register(&host, c"libopenmpt", &mut settings),
            RVSettingsResult::DuplicatedId as u32
        );

        let vtable = settings_vtable(&host);
        // SAFETY: every string passed is a live NUL-terminated literal.
        unsafe {
            let get_int = vtable.get_int.expect("get_int");
            assert_eq!(
                get_int(
                    vtable.private_data,
                    c"nope".as_ptr(),
                    c"".as_ptr(),
                    c"separation".as_ptr()
                )
                .result,
                RVSettingsResult::NotFound as u32
            );
            assert_eq!(
                get_int(
                    vtable.private_data,
                    c"libopenmpt".as_ptr(),
                    c"".as_ptr(),
                    c"nope".as_ptr()
                )
                .result,
                RVSettingsResult::UnknownId as u32
            );
            assert_eq!(
                get_int(
                    vtable.private_data,
                    c"libopenmpt".as_ptr(),
                    c"".as_ptr(),
                    c"gain".as_ptr()
                )
                .result,
                RVSettingsResult::WrongType as u32
            );
        }
    }

    #[test]
    fn each_accessor_reads_its_own_kind_and_honours_the_extension_override() {
        let mut int_choices = [RVSIntegerRangeValue {
            name: c"Stereo".as_ptr(),
            value: 2,
        }];
        let mut str_choices = [RVSStringRangeValue {
            name: c"A500".as_ptr(),
            value: c"a500".as_ptr(),
        }];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);
        let host = ServiceHost::default();
        register(&host, c"libopenmpt", &mut settings);

        host.settings()
            .set("libopenmpt", "mod", "separation", Value::Int(150))
            .expect("override");

        let vtable = settings_vtable(&host);
        // SAFETY: every string passed is a live NUL-terminated literal.
        unsafe {
            let float = (vtable.get_float.expect("get_float"))(
                vtable.private_data,
                c"libopenmpt".as_ptr(),
                c"".as_ptr(),
                c"gain".as_ptr(),
            );
            assert_eq!(
                (float.result, float.value),
                (RVSettingsResult::Ok as u32, 100.0)
            );

            let boolean = (vtable.get_bool.expect("get_bool"))(
                vtable.private_data,
                c"libopenmpt".as_ptr(),
                c"".as_ptr(),
                c"amiga".as_ptr(),
            );
            assert_eq!(
                (boolean.result, boolean.value),
                (RVSettingsResult::Ok as u32, true)
            );

            let get_int = vtable.get_int.expect("get_int");
            let global = get_int(
                vtable.private_data,
                c"libopenmpt".as_ptr(),
                c"".as_ptr(),
                c"separation".as_ptr(),
            );
            assert_eq!(
                (global.result, global.value),
                (RVSettingsResult::Ok as u32, 100)
            );

            let overridden = get_int(
                vtable.private_data,
                c"libopenmpt".as_ptr(),
                c"mod".as_ptr(),
                c"separation".as_ptr(),
            );
            assert_eq!(
                (overridden.result, overridden.value),
                (RVSettingsResult::Ok as u32, 150)
            );

            let other = get_int(
                vtable.private_data,
                c"libopenmpt".as_ptr(),
                c"ahx".as_ptr(),
                c"separation".as_ptr(),
            );
            assert_eq!(
                (other.result, other.value),
                (RVSettingsResult::Ok as u32, 100)
            );
        }
    }

    #[test]
    fn a_string_read_outlives_its_call_and_repeats_the_same_pointer() {
        let mut int_choices = [];
        let mut str_choices = [RVSStringRangeValue {
            name: c"A500".as_ptr(),
            value: c"a500".as_ptr(),
        }];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);
        let host = ServiceHost::default();
        register(&host, c"libopenmpt", &mut settings);

        let vtable = settings_vtable(&host);
        let read = || {
            // SAFETY: every string passed is a live NUL-terminated literal.
            unsafe {
                (vtable.get_string.expect("get_string"))(
                    vtable.private_data,
                    c"libopenmpt".as_ptr(),
                    c"".as_ptr(),
                    c"filter".as_ptr(),
                )
            }
        };

        // SAFETY: the registry owns every string it hands out for as long as the host lives.
        let text = |result: RVSStringResult| unsafe { CStr::from_ptr(result.value) };

        let first = read();
        assert_eq!(first.result, RVSettingsResult::Ok as u32);
        assert_eq!(text(first), c"auto");

        // Polling the same value must not grow the registry's string storage.
        assert_eq!(read().value, first.value);

        host.settings()
            .set("libopenmpt", "", "filter", Value::Str("a500".to_string()))
            .expect("write");
        assert_eq!(text(read()), c"a500");
        assert_eq!(text(first), c"auto");
    }

    #[test]
    fn null_strings_from_a_plugin_are_refused_and_a_null_ext_reads_as_global() {
        let mut int_choices = [];
        let mut str_choices = [];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);
        let host = ServiceHost::default();
        register(&host, c"libopenmpt", &mut settings);

        let vtable = settings_vtable(&host);
        // SAFETY: null is explicitly allowed, and the rest are live literals.
        unsafe {
            let reg = vtable.reg.expect("reg");
            assert_eq!(
                reg(
                    vtable.private_data,
                    ptr::null(),
                    settings.as_mut_ptr(),
                    settings.len() as u64
                ),
                RVSettingsResult::NotFound as u32
            );
            assert_eq!(
                reg(
                    vtable.private_data,
                    c"".as_ptr(),
                    settings.as_mut_ptr(),
                    settings.len() as u64
                ),
                RVSettingsResult::NotFound as u32
            );
            assert_eq!(host.settings().reg_ids(), ["libopenmpt"]);

            let get_int = vtable.get_int.expect("get_int");
            assert_eq!(
                get_int(
                    vtable.private_data,
                    ptr::null(),
                    c"".as_ptr(),
                    c"separation".as_ptr()
                )
                .result,
                RVSettingsResult::NotFound as u32
            );
            assert_eq!(
                get_int(
                    vtable.private_data,
                    c"libopenmpt".as_ptr(),
                    c"".as_ptr(),
                    ptr::null()
                )
                .result,
                RVSettingsResult::NotFound as u32
            );

            // A null ext is the same request as an empty one: the global value.
            host.settings()
                .set("libopenmpt", "mod", "separation", Value::Int(150))
                .expect("override");
            let null_ext = get_int(
                vtable.private_data,
                c"libopenmpt".as_ptr(),
                ptr::null(),
                c"separation".as_ptr(),
            );
            assert_eq!(
                (null_ext.result, null_ext.value),
                (RVSettingsResult::Ok as u32, 100)
            );
        }
    }

    #[test]
    fn a_registration_carrying_no_settings_is_accepted_and_stays_empty() {
        let host = ServiceHost::default();
        let vtable = settings_vtable(&host);

        // SAFETY: a null array with a zero count, and a null array whose count lies about
        // being populated — both are things a buggy plugin can hand us.
        unsafe {
            let reg = vtable.reg.expect("reg");
            assert_eq!(
                reg(vtable.private_data, c"empty".as_ptr(), ptr::null_mut(), 0),
                RVSettingsResult::Ok as u32
            );
            assert_eq!(
                reg(vtable.private_data, c"lying".as_ptr(), ptr::null_mut(), 4),
                RVSettingsResult::Ok as u32
            );
        }

        let settings = host.settings();
        assert_eq!(settings.reg_ids(), ["empty", "lying"]);
        assert!(settings.schemas("empty").is_empty());
        assert!(settings.schemas("lying").is_empty());
    }

    #[test]
    fn a_string_that_is_not_utf8_is_replaced_rather_than_rejected() {
        let reg_id = CString::new(vec![b'u', 0xFF, b'e']).expect("reg_id");
        let name = CString::new(vec![b'G', 0xFF, b'n']).expect("name");
        let mut settings = [RVSetting {
            int_value: RVSInteger {
                widget_id: c"filter".as_ptr(),
                name: name.as_ptr(),
                desc: c"".as_ptr(),
                widget_type: RVS_INTEGER_TYPE,
                value: 1,
                start_range: 0,
                end_range: 0,
            },
        }];

        let host = ServiceHost::default();
        assert_eq!(
            register(&host, &reg_id, &mut settings),
            RVSettingsResult::Ok as u32
        );

        let lossy_reg_id = "u\u{FFFD}e";
        let settings = host.settings();
        assert_eq!(settings.reg_ids(), [lossy_reg_id]);
        assert_eq!(settings.schemas(lossy_reg_id)[0].name, "G\u{FFFD}n");
    }

    #[test]
    fn a_plugin_keeps_reading_the_registry_while_another_thread_writes_it() {
        let mut int_choices = [];
        let mut str_choices = [];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);
        let host = ServiceHost::default();
        register(&host, c"libopenmpt", &mut settings);

        const WRITES: i32 = 500;
        let writer = host.settings();
        let writing = std::thread::spawn(move || {
            for value in 101..=WRITES {
                writer
                    .set("libopenmpt", "", "separation", Value::Int(value))
                    .expect("write");
            }
        });

        let vtable = settings_vtable(&host);
        for _ in 0..WRITES {
            // SAFETY: every string passed is a live NUL-terminated literal.
            let read = unsafe {
                (vtable.get_int.expect("get_int"))(
                    vtable.private_data,
                    c"libopenmpt".as_ptr(),
                    c"".as_ptr(),
                    c"separation".as_ptr(),
                )
            };
            assert_eq!(read.result, RVSettingsResult::Ok as u32);
            assert!(
                (100..=WRITES).contains(&read.value),
                "torn read {}",
                read.value
            );
        }

        writing.join().expect("writer thread");
        assert_eq!(
            host.settings().get("libopenmpt", "", "separation"),
            Ok(Value::Int(WRITES))
        );
    }

    #[test]
    fn a_panicking_host_service_fails_the_call_instead_of_the_process() {
        let host = ServiceHost::new(
            Box::new(PanickingIo),
            Arc::new(PanickingLog),
            Box::new(PanickingStore),
        );
        let mut int_choices = [];
        let mut str_choices = [];
        let mut settings = every_widget_kind(&mut int_choices, &mut str_choices);

        let io = io_vtable(&host);
        let url = CString::new("song.mod").expect("url");
        // SAFETY: every pointer passed is live for the duration of the call.
        unsafe {
            assert!(!(io.exists.expect("exists"))(io.private_data, url.as_ptr()));
            let read = (io.read_url_to_memory.expect("read"))(io.private_data, url.as_ptr());
            assert!(read.data.is_null());

            let service = host.service();
            let log =
                &*(service.get_log.expect("get_log"))(service.private_data, RV_LOG_API_VERSION);
            (log.log.expect("log"))(
                log.private_data,
                RVLogLevel::Error as u32,
                ptr::null(),
                0,
                c"still alive".as_ptr(),
            );
        }

        // The store panics while loading, which happens inside `reg`.
        assert_eq!(
            register(&host, c"libopenmpt", &mut settings),
            RVSettingsResult::NotFound as u32
        );
    }
}

/// The services as a real C plugin reaches them: through the header macros, with settings
/// built by the header's own initializer macros.
#[cfg(all(test, unix))]
mod c_tests {
    use super::tests::{with_log, CapturingLog};
    use super::*;

    use core::ffi::CStr;
    use std::ffi::CString;

    use libloading::{Library, Symbol};

    use crate::c_fixture::compile;
    use crate::service::settings::Choice;

    const FIXTURE: &str = r#"
#include <stddef.h>

#include <retrovert/io.h>
#include <retrovert/log.h>
#include <retrovert/service.h>
#include <retrovert/settings.h>

RV_PLUGIN_USE_IO_API();
RV_PLUGIN_USE_LOG_API();
static const RVSettings* g_settings = NULL;

static RVSIntegerRangeValue s_channel_values[] = { { "Mono", 1 }, { "Stereo", 2 } };
static RVSStringRangeValue s_filter_values[] = { { "Auto", "auto" }, { "A500", "a500" } };

static RVSetting s_settings[] = {
    RVSIntValue_Range("separation", "Stereo Separation", "percent", 100, 0, 200),
    RVSFloatValue_Range("gain", "Master Gain", "percent", 100.0f, 0.0f, 400.0f),
    RVSBoolValue("amiga", "Amiga Resampling", "emulate", true),
    RVSIntValue_DescRange("channels", "Channels", "", 2, s_channel_values),
    RVSStringValue_DescRange("filter", "Filter", "", "auto", s_filter_values),
};

int fixture_init(const RVService* services) {
    rv_init_io_api(services);
    rv_init_log_api(services);
    g_settings = RVService_get_settings(services, RV_SETTINGS_API_VERSION);
    if (!g_rv_io || !g_rv_log || !g_settings) {
        return -1;
    }
    return (int)RVSettings_register_array(g_settings, "fixture", s_settings);
}

int fixture_exists(const char* url) { return rv_io_exists(url) ? 1 : 0; }

uint64_t fixture_read(const char* url) {
    RVIoReadUrlResult result = rv_io_read_url_to_memory(url);
    uint64_t size = result.data_size;
    if (result.data) {
        rv_io_free_url_to_memory(result.data);
    }
    return size;
}

void fixture_log(void) { rvfl_warn("cannot open %s (%d)", "song.mod", 7); }

int fixture_separation(const char* ext) {
    RVSIntResult result = RVSettings_get_int(g_settings, "fixture", ext, "separation");
    return result.result == RVSettingsResult_Ok ? result.value : -1;
}

const char* fixture_filter(void) {
    RVSStringResult result = RVSettings_get_string(g_settings, "fixture", "", "filter");
    return result.result == RVSettingsResult_Ok ? result.value : NULL;
}
"#;

    #[test]
    fn a_c_plugin_registers_reads_files_logs_and_sees_host_writes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let library = compile(dir.path(), "service_fixture", FIXTURE);
        // SAFETY: the fixture was just built from the source above and runs no static
        // initializers of its own.
        let library = unsafe { Library::new(&library) }.expect("load fixture");

        let captured = Arc::new(CapturingLog::default());
        let host = with_log(captured.clone());

        // SAFETY: each symbol is looked up with the signature the fixture defines, and the
        // strings passed live across every call.
        unsafe {
            let init: Symbol<unsafe extern "C" fn(*const RVService) -> c_int> =
                library.get(b"fixture_init").expect("fixture_init");
            assert_eq!(init(host.service()), 0, "registration failed");

            let settings = host.settings();
            let schemas = settings.schemas("fixture");
            assert_eq!(
                schemas.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
                ["separation", "gain", "amiga", "channels", "filter"]
            );
            assert_eq!(schemas[0].name, "Stereo Separation");
            assert_eq!(schemas[0].desc, "percent");
            assert_eq!(
                schemas[0].setting,
                Setting::Int {
                    value: 100,
                    start_range: 0,
                    end_range: 200
                }
            );
            assert_eq!(
                schemas[1].setting,
                Setting::Float {
                    value: 100.0,
                    start_range: 0.0,
                    end_range: 400.0
                }
            );
            assert_eq!(schemas[2].setting, Setting::Bool { value: true });
            assert_eq!(
                schemas[3].setting,
                Setting::IntChoice {
                    value: 2,
                    choices: vec![
                        Choice {
                            name: "Mono".to_string(),
                            value: 1
                        },
                        Choice {
                            name: "Stereo".to_string(),
                            value: 2
                        },
                    ]
                }
            );
            assert_eq!(
                schemas[4].setting,
                Setting::StrChoice {
                    value: "auto".to_string(),
                    choices: vec![
                        Choice {
                            name: "Auto".to_string(),
                            value: "auto".to_string()
                        },
                        Choice {
                            name: "A500".to_string(),
                            value: "a500".to_string()
                        },
                    ]
                }
            );

            let path = dir.path().join("song.mod");
            std::fs::write(&path, b"payload").expect("write");
            let url = CString::new(path.to_str().expect("utf-8 path")).expect("url");
            let missing = CString::new("/definitely/not/here.mod").expect("url");

            let exists: Symbol<unsafe extern "C" fn(*const c_char) -> c_int> =
                library.get(b"fixture_exists").expect("fixture_exists");
            assert_eq!(exists(url.as_ptr()), 1);
            assert_eq!(exists(missing.as_ptr()), 0);

            let read: Symbol<unsafe extern "C" fn(*const c_char) -> u64> =
                library.get(b"fixture_read").expect("fixture_read");
            assert_eq!(read(url.as_ptr()), 7);
            assert_eq!(read(missing.as_ptr()), 0);

            let log: Symbol<unsafe extern "C" fn()> =
                library.get(b"fixture_log").expect("fixture_log");
            log();

            // A host write reaches the plugin, per extension and globally.
            settings
                .set("fixture", "mod", "separation", Value::Int(150))
                .expect("override");
            settings
                .set("fixture", "", "filter", Value::Str("a500".to_string()))
                .expect("global");

            let separation: Symbol<unsafe extern "C" fn(*const c_char) -> c_int> = library
                .get(b"fixture_separation")
                .expect("fixture_separation");
            assert_eq!(separation(c"".as_ptr()), 100);
            assert_eq!(separation(c"mod".as_ptr()), 150);
            assert_eq!(separation(c"ahx".as_ptr()), 100);

            let filter: Symbol<unsafe extern "C" fn() -> *const c_char> =
                library.get(b"fixture_filter").expect("fixture_filter");
            assert_eq!(CStr::from_ptr(filter()), c"a500");
        }

        let lines = captured.lines.lock().expect("log lines");
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with("Warn ") && lines[0].ends_with("cannot open song.mod (7)"),
            "{lines:?}"
        );
        assert!(lines[0].contains("service_fixture.c:"), "{lines:?}");
    }
}
