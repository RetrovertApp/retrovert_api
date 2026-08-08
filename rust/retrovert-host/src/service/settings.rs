//! The settings service: the registry a plugin registers into and a host renders from.
//!
//! The crate owns the bookkeeping — schema storage, duplicate detection, type checking and
//! per-extension resolution — so every host gets the same answers. A host supplies only
//! persistence, through [`SettingsStore`].

use core::ffi::{c_char, c_void};
use core::ptr;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::ffi::settings::{
    RVSBase, RVSBool, RVSBoolResult, RVSFloat, RVSFloatResult, RVSIntResult, RVSInteger,
    RVSIntegerFixedRange, RVSIntegerRangeValue, RVSStringFixedRange, RVSStringRangeValue,
    RVSStringResult, RVSetting, RVSettingsResult, RVS_BOOL_TYPE, RVS_FLOAT_TYPE,
    RVS_INTEGER_RANGE_TYPE, RVS_INTEGER_TYPE, RVS_STRING_RANGE_TYPE,
};
use crate::service::log::{Log, LogLevel};
use crate::service::{context, guard, plugin_str};

/// One entry of a fixed-range setting: a display name for a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice<T> {
    pub name: String,
    pub value: T,
}

/// The value a setting holds. Closed, because the ABI is frozen at these five kinds.
#[derive(Debug, Clone, PartialEq)]
pub enum Setting {
    /// `start_range == end_range` means the plugin declared no bounds.
    Float {
        value: f32,
        start_range: f32,
        end_range: f32,
    },
    /// `start_range == end_range` means the plugin declared no bounds.
    Int {
        value: i32,
        start_range: i32,
        end_range: i32,
    },
    Bool {
        value: bool,
    },
    IntChoice {
        value: i32,
        choices: Vec<Choice<i32>>,
    },
    StrChoice {
        value: String,
        choices: Vec<Choice<String>>,
    },
}

impl Setting {
    /// The name of the accessor that reads this kind, for error messages.
    fn type_name(&self) -> &'static str {
        match self {
            Self::Float { .. } => "float",
            Self::Int { .. } | Self::IntChoice { .. } => "int",
            Self::Bool { .. } => "bool",
            Self::StrChoice { .. } => "string",
        }
    }

    fn value(&self) -> Value {
        match self {
            Self::Float { value, .. } => Value::Float(*value),
            Self::Int { value, .. } | Self::IntChoice { value, .. } => Value::Int(*value),
            Self::Bool { value } => Value::Bool(*value),
            Self::StrChoice { value, .. } => Value::Str(value.clone()),
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        matches!(
            (self, value),
            (Self::Float { .. }, Value::Float(_))
                | (Self::Int { .. } | Self::IntChoice { .. }, Value::Int(_))
                | (Self::Bool { .. }, Value::Bool(_))
                | (Self::StrChoice { .. }, Value::Str(_))
        )
    }

    fn assign(&mut self, new: Value) {
        match (self, new) {
            (Self::Float { value, .. }, Value::Float(new)) => *value = new,
            (Self::Int { value, .. } | Self::IntChoice { value, .. }, Value::Int(new)) => {
                *value = new
            }
            (Self::Bool { value }, Value::Bool(new)) => *value = new,
            (Self::StrChoice { value, .. }, Value::Str(new)) => *value = new,
            _ => unreachable!("assign runs only after accepts"),
        }
    }
}

/// A registered setting: its identity and description, plus the value it holds.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingSchema {
    pub id: String,
    pub name: String,
    pub desc: String,
    pub setting: Setting,
}

/// A setting's value, as it crosses the host-facing surface and the persistence store.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Float(f32),
    Int(i32),
    Bool(bool),
    Str(String),
}

impl Value {
    fn type_name(&self) -> &'static str {
        match self {
            Self::Float(_) => "float",
            Self::Int(_) => "int",
            Self::Bool(_) => "bool",
            Self::Str(_) => "string",
        }
    }
}

/// One value as persistence sees it. `ext` is empty for the global value.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredValue {
    pub ext: String,
    pub id: String,
    pub value: Value,
}

/// Whether a settings change can be applied live or needs the song reopened.
///
/// Nothing can apply a change to an already-open song until its plugin instance says so,
/// which is what the plugin's `settings_updated` callback answers; until then a real change
/// reports [`SettingsUpdate::RequireRestart`] and a write that changed nothing reports
/// [`SettingsUpdate::Default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsUpdate {
    Default,
    RequireRestart,
}

/// Where a host keeps settings between runs.
///
/// Dumb by design: the crate resolves and type-checks, the store only moves values. The
/// crate picks no file format — [`load`](SettingsStore::load) and
/// [`save`](SettingsStore::save) work in whole per-`reg_id` sets.
pub trait SettingsStore: Send + Sync {
    /// Called while the plugin registers, so its saved values are live from the first read.
    fn load(&self, reg_id: &str) -> Vec<StoredValue>;

    fn save(&self, reg_id: &str, values: &[StoredValue]);
}

/// The default [`SettingsStore`]: keeps values for the process, writes nothing to disk.
#[derive(Default)]
pub struct MemorySettingsStore {
    modules: Mutex<HashMap<String, Vec<StoredValue>>>,
}

impl SettingsStore for MemorySettingsStore {
    fn load(&self, reg_id: &str) -> Vec<StoredValue> {
        lock(&self.modules).get(reg_id).cloned().unwrap_or_default()
    }

    fn save(&self, reg_id: &str, values: &[StoredValue]) {
        lock(&self.modules).insert(reg_id.to_string(), values.to_vec());
    }
}

/// Why a registration or a lookup did not succeed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("nothing is registered under '{reg_id}'")]
    NotFound { reg_id: String },
    #[error("'{reg_id}' has no setting '{id}'")]
    UnknownId { reg_id: String, id: String },
    #[error("setting '{reg_id}.{id}' is a {actual}, not a {requested}")]
    WrongType {
        reg_id: String,
        id: String,
        actual: &'static str,
        requested: &'static str,
    },
    #[error("setting '{reg_id}.{id}' cannot hold a string with an interior NUL")]
    InteriorNul { reg_id: String, id: String },
    #[error("'{reg_id}' is already registered")]
    DuplicateRegId { reg_id: String },
    #[error("'{reg_id}' registers '{id}' twice")]
    DuplicateId { reg_id: String, id: String },
}

impl SettingsError {
    fn to_abi(&self) -> RVSettingsResult {
        match self {
            Self::NotFound { .. } => RVSettingsResult::NotFound,
            Self::UnknownId { .. } => RVSettingsResult::UnknownId,
            Self::WrongType { .. } | Self::InteriorNul { .. } => RVSettingsResult::WrongType,
            Self::DuplicateRegId { .. } | Self::DuplicateId { .. } => {
                RVSettingsResult::DuplicatedId
            }
        }
    }
}

/// Extensions are compared without a leading dot and without case, so a setting stored for
/// `.MOD` answers a lookup for `mod`.
fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_lowercase()
}

/// A string with an interior NUL cannot reach a plugin as a `const char*`, so the registry
/// refuses it at the door rather than handing back a truncated or null value later.
fn nul_free(value: &Value) -> bool {
    match value {
        Value::Str(text) => !text.as_bytes().contains(&0),
        _ => true,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One plugin's settings: the schemas it registered, plus per-extension overrides.
struct Module {
    reg_id: String,
    schemas: Vec<SettingSchema>,
    overrides: Vec<StoredValue>,
    dirty: bool,
}

impl Module {
    fn schema(&self, id: &str) -> Option<&SettingSchema> {
        self.schemas.iter().find(|schema| schema.id == id)
    }

    fn stored_values(&self) -> Vec<StoredValue> {
        self.schemas
            .iter()
            .map(|schema| StoredValue {
                ext: String::new(),
                id: schema.id.clone(),
                value: schema.setting.value(),
            })
            .chain(self.overrides.iter().cloned())
            .collect()
    }
}

struct Registry {
    modules: Vec<Module>,
    store: Box<dyn SettingsStore>,
    log: Arc<dyn Log>,
    /// String values handed to plugins as `const char*`. Append-only and deduplicated, so a
    /// pointer stays valid for as long as the registry and repeated reads do not grow it.
    interned: Vec<CString>,
}

impl Registry {
    fn module(&self, reg_id: &str) -> Result<&Module, SettingsError> {
        self.modules
            .iter()
            .find(|module| module.reg_id == reg_id)
            .ok_or_else(|| SettingsError::NotFound {
                reg_id: reg_id.to_string(),
            })
    }

    fn register(&mut self, reg_id: &str, schemas: Vec<SettingSchema>) -> Result<(), SettingsError> {
        if self.modules.iter().any(|module| module.reg_id == reg_id) {
            return Err(SettingsError::DuplicateRegId {
                reg_id: reg_id.to_string(),
            });
        }
        for (index, schema) in schemas.iter().enumerate() {
            if schemas[..index].iter().any(|prior| prior.id == schema.id) {
                return Err(SettingsError::DuplicateId {
                    reg_id: reg_id.to_string(),
                    id: schema.id.clone(),
                });
            }
        }

        let mut module = Module {
            reg_id: reg_id.to_string(),
            schemas,
            overrides: Vec::new(),
            dirty: false,
        };
        for stored in self.store.load(reg_id) {
            self.restore(&mut module, stored);
        }
        self.modules.push(module);
        Ok(())
    }

    /// Applies one persisted value over a freshly registered schema. A value the schema no
    /// longer matches is reported and dropped — settings outlive the plugins that wrote them.
    fn restore(&self, module: &mut Module, stored: StoredValue) {
        let Some(schema) = module.schema(&stored.id) else {
            self.log.log(
                LogLevel::Warn,
                None,
                0,
                &format!(
                    "'{}' has no setting '{}'; stored value dropped",
                    module.reg_id, stored.id
                ),
            );
            return;
        };
        if !schema.setting.accepts(&stored.value) {
            self.log.log(
                LogLevel::Warn,
                None,
                0,
                &format!(
                    "'{}.{}' is a {}, not a {}; stored value dropped",
                    module.reg_id,
                    stored.id,
                    schema.setting.type_name(),
                    stored.value.type_name()
                ),
            );
            return;
        }
        if !nul_free(&stored.value) {
            self.log.log(
                LogLevel::Warn,
                None,
                0,
                &format!(
                    "'{}.{}' holds a string with an interior NUL; stored value dropped",
                    module.reg_id, stored.id
                ),
            );
            return;
        }
        write(
            module,
            &normalize_ext(&stored.ext),
            &stored.id,
            stored.value,
        );
    }

    fn get(&self, reg_id: &str, ext: &str, id: &str) -> Result<Value, SettingsError> {
        let module = self.module(reg_id)?;
        let schema = module.schema(id).ok_or_else(|| SettingsError::UnknownId {
            reg_id: reg_id.to_string(),
            id: id.to_string(),
        })?;

        let ext = normalize_ext(ext);
        if !ext.is_empty() {
            if let Some(over) = module
                .overrides
                .iter()
                .find(|over| over.ext == ext && over.id == id)
            {
                return Ok(over.value.clone());
            }
        }
        Ok(schema.setting.value())
    }

    fn set(
        &mut self,
        reg_id: &str,
        ext: &str,
        id: &str,
        value: Value,
    ) -> Result<SettingsUpdate, SettingsError> {
        let module = self
            .modules
            .iter_mut()
            .find(|module| module.reg_id == reg_id)
            .ok_or_else(|| SettingsError::NotFound {
                reg_id: reg_id.to_string(),
            })?;
        let schema = module.schema(id).ok_or_else(|| SettingsError::UnknownId {
            reg_id: reg_id.to_string(),
            id: id.to_string(),
        })?;
        if !schema.setting.accepts(&value) {
            return Err(SettingsError::WrongType {
                reg_id: reg_id.to_string(),
                id: id.to_string(),
                actual: schema.setting.type_name(),
                requested: value.type_name(),
            });
        }
        if !nul_free(&value) {
            return Err(SettingsError::InteriorNul {
                reg_id: reg_id.to_string(),
                id: id.to_string(),
            });
        }
        let global = schema.setting.value();

        // Only an equal value at the same scope is a no-op: pinning an extension to the
        // global value still has to stop a later global write from moving it.
        let ext = normalize_ext(ext);
        let unchanged = if ext.is_empty() {
            global == value
        } else {
            module
                .overrides
                .iter()
                .any(|over| over.ext == ext && over.id == id && over.value == value)
        };
        if unchanged {
            return Ok(SettingsUpdate::Default);
        }

        write(module, &ext, id, value);
        module.dirty = true;
        Ok(SettingsUpdate::RequireRestart)
    }

    fn flush(&mut self) {
        let Self { modules, store, .. } = self;
        for module in modules.iter_mut().filter(|module| module.dirty) {
            store.save(&module.reg_id, &module.stored_values());
            module.dirty = false;
        }
    }

    /// Returns a `const char*` that outlives the call, reusing an equal string if one was
    /// already handed out.
    fn intern(&mut self, value: &str) -> *const c_char {
        let owned = CString::new(value).expect("registry strings are checked NUL-free on entry");
        let index = self
            .interned
            .iter()
            .position(|existing| *existing == owned)
            .unwrap_or_else(|| {
                self.interned.push(owned);
                self.interned.len() - 1
            });
        self.interned[index].as_ptr()
    }
}

/// Writes a value that the schema has already accepted: into the schema itself when `ext`
/// is empty, otherwise into the per-extension override list.
fn write(module: &mut Module, ext: &str, id: &str, value: Value) {
    if ext.is_empty() {
        module
            .schemas
            .iter_mut()
            .find(|schema| schema.id == id)
            .expect("the schema resolved before the write")
            .setting
            .assign(value);
        return;
    }
    match module
        .overrides
        .iter_mut()
        .find(|over| over.ext == ext && over.id == id)
    {
        Some(over) => over.value = value,
        None => module.overrides.push(StoredValue {
            ext: ext.to_string(),
            id: id.to_string(),
            value,
        }),
    }
}

/// The host's half of the settings service: what a settings page enumerates, reads and writes.
///
/// Cloning shares one registry; plugins read it from their decode thread while the UI writes it.
#[derive(Clone)]
pub struct SettingsHandle {
    registry: Arc<Mutex<Registry>>,
}

impl SettingsHandle {
    pub(super) fn new(store: Box<dyn SettingsStore>, log: Arc<dyn Log>) -> Self {
        Self {
            registry: Arc::new(Mutex::new(Registry {
                modules: Vec::new(),
                store,
                log,
                interned: Vec::new(),
            })),
        }
    }

    /// Registers a module's schemas and immediately overlays whatever the store holds for it.
    pub fn register(&self, reg_id: &str, schemas: Vec<SettingSchema>) -> Result<(), SettingsError> {
        lock(&self.registry).register(reg_id, schemas)
    }

    /// Registered modules, in registration order.
    pub fn reg_ids(&self) -> Vec<String> {
        lock(&self.registry)
            .modules
            .iter()
            .map(|module| module.reg_id.clone())
            .collect()
    }

    /// A module's schemas with their current global values; empty when nothing is registered.
    pub fn schemas(&self, reg_id: &str) -> Vec<SettingSchema> {
        let registry = lock(&self.registry);
        registry
            .module(reg_id)
            .map(|module| module.schemas.clone())
            .unwrap_or_default()
    }

    /// Reads the value for `ext`, falling back to the global value. `ext` is empty for global.
    pub fn get(&self, reg_id: &str, ext: &str, id: &str) -> Result<Value, SettingsError> {
        lock(&self.registry).get(reg_id, ext, id)
    }

    /// Writes the value for `ext`, or the global value when `ext` is empty.
    pub fn set(
        &self,
        reg_id: &str,
        ext: &str,
        id: &str,
        value: Value,
    ) -> Result<SettingsUpdate, SettingsError> {
        lock(&self.registry).set(reg_id, ext, id, value)
    }

    /// Whether any module has changed since it was last written to the store.
    pub fn is_dirty(&self) -> bool {
        lock(&self.registry)
            .modules
            .iter()
            .any(|module| module.dirty)
    }

    /// Writes every changed module to the store. Hosts choose when; the crate never does it.
    pub fn flush(&self) {
        lock(&self.registry).flush()
    }
}

/// Reads the schemas out of the array a plugin registers.
///
/// # Safety
///
/// `settings` must be null, or address `count` initialized [`RVSetting`] values whose string
/// and choice-array pointers are readable for their whole lengths.
unsafe fn read_schemas(
    log: &dyn Log,
    reg_id: &str,
    settings: *mut RVSetting,
    count: u64,
) -> Vec<SettingSchema> {
    if settings.is_null() {
        return Vec::new();
    }

    let mut schemas = Vec::new();
    for index in 0..count {
        // SAFETY: the caller guarantees `count` initialized values from `settings`.
        let entry = unsafe { settings.add(index as usize) };
        // SAFETY: every union member starts with the fields of `RVSBase`.
        let base = unsafe { entry.cast::<RVSBase>().read() };
        // SAFETY: the caller guarantees readable strings.
        let (id, name, desc) = unsafe {
            (
                plugin_str(base.widget_id),
                plugin_str(base.name),
                plugin_str(base.desc),
            )
        };
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            log.log(
                LogLevel::Warn,
                None,
                0,
                &format!("'{reg_id}' registered a setting without an id; skipped"),
            );
            continue;
        };

        // SAFETY: `widget_type` names the union member the plugin initialized, and every
        // member of a `#[repr(C)]` union starts at the union's own address.
        let setting = unsafe {
            match base.widget_type {
                RVS_FLOAT_TYPE => {
                    let raw = entry.cast::<RVSFloat>().read();
                    Some(Setting::Float {
                        value: raw.value,
                        start_range: raw.start_range,
                        end_range: raw.end_range,
                    })
                }
                RVS_INTEGER_TYPE => {
                    let raw = entry.cast::<RVSInteger>().read();
                    Some(Setting::Int {
                        value: raw.value,
                        start_range: raw.start_range,
                        end_range: raw.end_range,
                    })
                }
                RVS_BOOL_TYPE => Some(Setting::Bool {
                    value: entry.cast::<RVSBool>().read().value,
                }),
                RVS_INTEGER_RANGE_TYPE => {
                    let raw = entry.cast::<RVSIntegerFixedRange>().read();
                    Some(Setting::IntChoice {
                        value: raw.value,
                        choices: read_int_choices(raw.values, raw.values_size),
                    })
                }
                RVS_STRING_RANGE_TYPE => {
                    let raw = entry.cast::<RVSStringFixedRange>().read();
                    Some(Setting::StrChoice {
                        value: plugin_str(raw.value).unwrap_or_default().into_owned(),
                        choices: read_str_choices(raw.values, raw.values_size),
                    })
                }
                _ => None,
            }
        };

        let Some(setting) = setting else {
            log.log(
                LogLevel::Warn,
                None,
                0,
                &format!(
                    "'{reg_id}.{id}' has unknown widget type {:#x}; skipped",
                    base.widget_type
                ),
            );
            continue;
        };

        schemas.push(SettingSchema {
            id: id.into_owned(),
            name: name.unwrap_or_default().into_owned(),
            desc: desc.unwrap_or_default().into_owned(),
            setting,
        });
    }
    schemas
}

/// # Safety
///
/// `values` must be null, or address `count` initialized entries whose names are readable.
unsafe fn read_int_choices(values: *mut RVSIntegerRangeValue, count: u64) -> Vec<Choice<i32>> {
    if values.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller guarantees `count` initialized entries from `values`.
    let entries = unsafe { core::slice::from_raw_parts(values, count as usize) };
    entries
        .iter()
        .filter_map(|entry| {
            // SAFETY: the caller guarantees a readable name.
            let name = unsafe { plugin_str(entry.name) }?;
            Some(Choice {
                name: name.into_owned(),
                value: entry.value,
            })
        })
        .collect()
}

/// # Safety
///
/// `values` must be null, or address `count` initialized entries whose strings are readable.
unsafe fn read_str_choices(values: *mut RVSStringRangeValue, count: u64) -> Vec<Choice<String>> {
    if values.is_null() {
        return Vec::new();
    }
    // SAFETY: the caller guarantees `count` initialized entries from `values`.
    let entries = unsafe { core::slice::from_raw_parts(values, count as usize) };
    entries
        .iter()
        .filter_map(|entry| {
            // SAFETY: the caller guarantees readable strings.
            let (name, value) = unsafe { (plugin_str(entry.name)?, plugin_str(entry.value)?) };
            Some(Choice {
                name: name.into_owned(),
                value: value.into_owned(),
            })
        })
        .collect()
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, `reg_id` must
/// be null or a readable NUL-terminated string, and `settings` must satisfy [`read_schemas`].
pub(super) unsafe extern "C" fn reg(
    private_data: *mut c_void,
    reg_id: *const c_char,
    settings: *mut RVSetting,
    settings_size: u64,
) -> u32 {
    guard(RVSettingsResult::NotFound as u32, || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, reg_id) = unsafe { (context(private_data), plugin_str(reg_id)) };
        let Some(reg_id) = reg_id.filter(|reg_id| !reg_id.is_empty()) else {
            return RVSettingsResult::NotFound as u32;
        };
        // SAFETY: the caller guarantees the settings array.
        let schemas = unsafe { read_schemas(&*ctx.log, &reg_id, settings, settings_size) };

        match ctx.settings.register(&reg_id, schemas) {
            Ok(()) => RVSettingsResult::Ok as u32,
            Err(error) => error.to_abi() as u32,
        }
    })
}

/// Resolves one plugin lookup, or the error code it should report.
///
/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and the three
/// strings must each be null or readable NUL-terminated strings.
unsafe fn lookup<'a>(
    private_data: *mut c_void,
    reg_id: *const c_char,
    ext: *const c_char,
    id: *const c_char,
) -> Result<(&'a SettingsHandle, Value), u32> {
    // SAFETY: the caller guarantees the context pointer and the strings.
    let (ctx, reg_id, ext, id) = unsafe {
        (
            context(private_data),
            plugin_str(reg_id),
            plugin_str(ext),
            plugin_str(id),
        )
    };
    let (Some(reg_id), Some(id)) = (reg_id, id) else {
        return Err(RVSettingsResult::NotFound as u32);
    };
    let value = ctx
        .settings
        .get(&reg_id, ext.as_deref().unwrap_or_default(), &id)
        .map_err(|error| error.to_abi() as u32)?;
    Ok((&ctx.settings, value))
}

/// # Safety
///
/// See [`lookup`].
pub(super) unsafe extern "C" fn get_string(
    private_data: *mut c_void,
    reg_id: *const c_char,
    ext: *const c_char,
    id: *const c_char,
) -> RVSStringResult {
    let failed = |result: u32| RVSStringResult {
        result,
        value: ptr::null(),
    };

    guard(failed(RVSettingsResult::NotFound as u32), || {
        // SAFETY: the caller upholds `lookup`'s contract.
        let (settings, value) = match unsafe { lookup(private_data, reg_id, ext, id) } {
            Ok(found) => found,
            Err(result) => return failed(result),
        };
        let Value::Str(value) = value else {
            return failed(RVSettingsResult::WrongType as u32);
        };
        RVSStringResult {
            result: RVSettingsResult::Ok as u32,
            value: lock(&settings.registry).intern(&value),
        }
    })
}

/// # Safety
///
/// See [`lookup`].
pub(super) unsafe extern "C" fn get_int(
    private_data: *mut c_void,
    reg_id: *const c_char,
    ext: *const c_char,
    id: *const c_char,
) -> RVSIntResult {
    let failed = |result: u32| RVSIntResult { result, value: 0 };

    guard(failed(RVSettingsResult::NotFound as u32), || {
        // SAFETY: the caller upholds `lookup`'s contract.
        let (_, value) = match unsafe { lookup(private_data, reg_id, ext, id) } {
            Ok(found) => found,
            Err(result) => return failed(result),
        };
        match value {
            Value::Int(value) => RVSIntResult {
                result: RVSettingsResult::Ok as u32,
                value,
            },
            _ => failed(RVSettingsResult::WrongType as u32),
        }
    })
}

/// # Safety
///
/// See [`lookup`].
pub(super) unsafe extern "C" fn get_float(
    private_data: *mut c_void,
    reg_id: *const c_char,
    ext: *const c_char,
    id: *const c_char,
) -> RVSFloatResult {
    let failed = |result: u32| RVSFloatResult { result, value: 0.0 };

    guard(failed(RVSettingsResult::NotFound as u32), || {
        // SAFETY: the caller upholds `lookup`'s contract.
        let (_, value) = match unsafe { lookup(private_data, reg_id, ext, id) } {
            Ok(found) => found,
            Err(result) => return failed(result),
        };
        match value {
            Value::Float(value) => RVSFloatResult {
                result: RVSettingsResult::Ok as u32,
                value,
            },
            _ => failed(RVSettingsResult::WrongType as u32),
        }
    })
}

/// # Safety
///
/// See [`lookup`].
pub(super) unsafe extern "C" fn get_bool(
    private_data: *mut c_void,
    reg_id: *const c_char,
    ext: *const c_char,
    id: *const c_char,
) -> RVSBoolResult {
    let failed = |result: u32| RVSBoolResult {
        result,
        value: false,
    };

    guard(failed(RVSettingsResult::NotFound as u32), || {
        // SAFETY: the caller upholds `lookup`'s contract.
        let (_, value) = match unsafe { lookup(private_data, reg_id, ext, id) } {
            Ok(found) => found,
            Err(result) => return failed(result),
        };
        match value {
            Value::Bool(value) => RVSBoolResult {
                result: RVSettingsResult::Ok as u32,
                value,
            },
            _ => failed(RVSettingsResult::WrongType as u32),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what the registry asked the store to do, and replays a canned load.
    #[derive(Default)]
    struct RecordingStore {
        loads: Mutex<Vec<StoredValue>>,
        saves: Mutex<Vec<(String, Vec<StoredValue>)>>,
    }

    impl SettingsStore for Arc<RecordingStore> {
        fn load(&self, _reg_id: &str) -> Vec<StoredValue> {
            lock(&self.loads).clone()
        }

        fn save(&self, reg_id: &str, values: &[StoredValue]) {
            lock(&self.saves).push((reg_id.to_string(), values.to_vec()));
        }
    }

    #[derive(Default)]
    struct CapturingLog {
        lines: Mutex<Vec<String>>,
    }

    impl Log for CapturingLog {
        fn log(&self, level: LogLevel, _file: Option<&str>, _line: u32, message: &str) {
            lock(&self.lines).push(format!("{level:?}: {message}"));
        }
    }

    fn schema(id: &str, setting: Setting) -> SettingSchema {
        SettingSchema {
            id: id.to_string(),
            name: id.to_string(),
            desc: String::new(),
            setting,
        }
    }

    fn int(value: i32) -> Setting {
        Setting::Int {
            value,
            start_range: 0,
            end_range: 0,
        }
    }

    fn handle() -> SettingsHandle {
        SettingsHandle::new(
            Box::new(MemorySettingsStore::default()),
            Arc::new(CapturingLog::default()),
        )
    }

    fn with_store(store: Arc<RecordingStore>, log: Arc<CapturingLog>) -> SettingsHandle {
        SettingsHandle::new(Box::new(store), log)
    }

    #[test]
    fn a_second_registration_under_the_same_id_is_refused() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("first registration");

        let error = settings
            .register("uade", vec![schema("other", int(2))])
            .expect_err("second registration");

        assert_eq!(
            error,
            SettingsError::DuplicateRegId {
                reg_id: "uade".to_string()
            }
        );
        assert_eq!(settings.schemas("uade").len(), 1);
    }

    #[test]
    fn one_registration_may_not_name_the_same_setting_twice() {
        let settings = handle();

        let error = settings
            .register(
                "uade",
                vec![schema("filter", int(1)), schema("filter", int(2))],
            )
            .expect_err("duplicate id");

        assert_eq!(
            error,
            SettingsError::DuplicateId {
                reg_id: "uade".to_string(),
                id: "filter".to_string()
            }
        );
        assert!(settings.reg_ids().is_empty());
    }

    #[test]
    fn stored_values_are_live_the_moment_a_plugin_registers() {
        let store = Arc::new(RecordingStore::default());
        *lock(&store.loads) = vec![
            StoredValue {
                ext: String::new(),
                id: "filter".to_string(),
                value: Value::Int(7),
            },
            StoredValue {
                ext: "mod".to_string(),
                id: "filter".to_string(),
                value: Value::Int(9),
            },
        ];
        let settings = with_store(Arc::clone(&store), Arc::new(CapturingLog::default()));

        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        assert_eq!(settings.get("uade", "", "filter"), Ok(Value::Int(7)));
        assert_eq!(settings.get("uade", "mod", "filter"), Ok(Value::Int(9)));
    }

    #[test]
    fn a_stored_value_the_schema_no_longer_matches_is_reported_and_dropped() {
        let store = Arc::new(RecordingStore::default());
        *lock(&store.loads) = vec![
            StoredValue {
                ext: String::new(),
                id: "gone".to_string(),
                value: Value::Int(7),
            },
            StoredValue {
                ext: String::new(),
                id: "filter".to_string(),
                value: Value::Bool(true),
            },
        ];
        let log = Arc::new(CapturingLog::default());
        let settings = with_store(Arc::clone(&store), Arc::clone(&log));

        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        assert_eq!(settings.get("uade", "", "filter"), Ok(Value::Int(1)));
        let lines = lock(&log.lines);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("no setting 'gone'"), "{lines:?}");
        assert!(lines[1].contains("is a int, not a bool"), "{lines:?}");
    }

    #[test]
    fn an_extension_override_shadows_the_global_value_only_for_that_extension() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        settings
            .set("uade", "mod", "filter", Value::Int(5))
            .expect("override");

        assert_eq!(settings.get("uade", "mod", "filter"), Ok(Value::Int(5)));
        assert_eq!(settings.get("uade", "ahx", "filter"), Ok(Value::Int(1)));
        assert_eq!(settings.get("uade", "", "filter"), Ok(Value::Int(1)));
    }

    #[test]
    fn changing_the_global_value_still_shows_through_extensions_without_an_override() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");
        settings
            .set("uade", "mod", "filter", Value::Int(5))
            .expect("override");

        settings
            .set("uade", "", "filter", Value::Int(3))
            .expect("global");

        assert_eq!(settings.get("uade", "mod", "filter"), Ok(Value::Int(5)));
        assert_eq!(settings.get("uade", "ahx", "filter"), Ok(Value::Int(3)));
    }

    #[test]
    fn extensions_match_without_a_leading_dot_and_without_case() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        settings
            .set("uade", ".MOD", "filter", Value::Int(5))
            .expect("override");

        assert_eq!(settings.get("uade", "mod", "filter"), Ok(Value::Int(5)));
        assert_eq!(settings.get("uade", "Mod", "filter"), Ok(Value::Int(5)));
    }

    #[test]
    fn an_unknown_module_a_missing_id_and_a_wrong_type_each_have_their_own_error() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        assert_eq!(
            settings.get("nope", "", "filter"),
            Err(SettingsError::NotFound {
                reg_id: "nope".to_string()
            })
        );
        assert_eq!(
            settings.get("uade", "", "nope"),
            Err(SettingsError::UnknownId {
                reg_id: "uade".to_string(),
                id: "nope".to_string()
            })
        );
        assert_eq!(
            settings.set("uade", "", "filter", Value::Bool(true)),
            Err(SettingsError::WrongType {
                reg_id: "uade".to_string(),
                id: "filter".to_string(),
                actual: "int",
                requested: "bool"
            })
        );
    }

    #[test]
    fn a_choice_setting_reads_and_writes_through_its_underlying_kind() {
        let settings = handle();
        settings
            .register(
                "openmpt",
                vec![
                    schema(
                        "channels",
                        Setting::IntChoice {
                            value: 2,
                            choices: vec![Choice {
                                name: "Stereo".to_string(),
                                value: 2,
                            }],
                        },
                    ),
                    schema(
                        "filter",
                        Setting::StrChoice {
                            value: "auto".to_string(),
                            choices: vec![Choice {
                                name: "Auto".to_string(),
                                value: "auto".to_string(),
                            }],
                        },
                    ),
                ],
            )
            .expect("registration");

        assert_eq!(settings.get("openmpt", "", "channels"), Ok(Value::Int(2)));
        settings
            .set("openmpt", "", "channels", Value::Int(4))
            .expect("write");
        assert_eq!(settings.get("openmpt", "", "channels"), Ok(Value::Int(4)));

        settings
            .set("openmpt", "", "filter", Value::Str("a500".to_string()))
            .expect("write");
        assert_eq!(
            settings.get("openmpt", "", "filter"),
            Ok(Value::Str("a500".to_string()))
        );
    }

    #[test]
    fn a_write_that_changes_nothing_neither_dirties_nor_asks_for_a_restart() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        assert_eq!(
            settings.set("uade", "", "filter", Value::Int(1)),
            Ok(SettingsUpdate::Default)
        );
        assert!(!settings.is_dirty());

        assert_eq!(
            settings.set("uade", "", "filter", Value::Int(2)),
            Ok(SettingsUpdate::RequireRestart)
        );
        assert!(settings.is_dirty());

        settings.flush();
        assert_eq!(
            settings.set("uade", "mod", "filter", Value::Int(5)),
            Ok(SettingsUpdate::RequireRestart)
        );
        settings.flush();
        assert_eq!(
            settings.set("uade", "mod", "filter", Value::Int(5)),
            Ok(SettingsUpdate::Default)
        );
        assert!(!settings.is_dirty());
    }

    #[test]
    fn flush_writes_changed_modules_once_and_carries_both_globals_and_overrides() {
        let store = Arc::new(RecordingStore::default());
        let settings = with_store(Arc::clone(&store), Arc::new(CapturingLog::default()));
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");
        settings
            .register("hively", vec![schema("volume", int(64))])
            .expect("registration");

        settings
            .set("uade", "mod", "filter", Value::Int(5))
            .expect("override");
        settings.flush();
        settings.flush();

        let saves = lock(&store.saves);
        assert_eq!(saves.len(), 1, "{saves:?}");
        assert_eq!(saves[0].0, "uade");
        assert_eq!(
            saves[0].1,
            vec![
                StoredValue {
                    ext: String::new(),
                    id: "filter".to_string(),
                    value: Value::Int(1)
                },
                StoredValue {
                    ext: "mod".to_string(),
                    id: "filter".to_string(),
                    value: Value::Int(5)
                },
            ]
        );
        assert!(!settings.is_dirty());
    }

    #[test]
    fn pinning_an_extension_to_the_global_value_survives_a_later_global_write() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");

        assert_eq!(
            settings.set("uade", "mod", "filter", Value::Int(1)),
            Ok(SettingsUpdate::RequireRestart)
        );
        settings
            .set("uade", "", "filter", Value::Int(3))
            .expect("global");

        assert_eq!(settings.get("uade", "mod", "filter"), Ok(Value::Int(1)));
        assert_eq!(settings.get("uade", "ahx", "filter"), Ok(Value::Int(3)));
    }

    #[test]
    fn a_string_with_an_interior_nul_is_refused_rather_than_handed_to_a_plugin() {
        let settings = handle();
        settings
            .register(
                "openmpt",
                vec![schema(
                    "filter",
                    Setting::StrChoice {
                        value: "auto".to_string(),
                        choices: Vec::new(),
                    },
                )],
            )
            .expect("registration");

        assert_eq!(
            settings.set("openmpt", "", "filter", Value::Str("a\x00500".to_string())),
            Err(SettingsError::InteriorNul {
                reg_id: "openmpt".to_string(),
                id: "filter".to_string()
            })
        );
        assert_eq!(
            settings.get("openmpt", "", "filter"),
            Ok(Value::Str("auto".to_string()))
        );
        assert!(!settings.is_dirty());
    }

    #[test]
    fn a_stored_string_with_an_interior_nul_is_reported_and_dropped() {
        let store = Arc::new(RecordingStore::default());
        *lock(&store.loads) = vec![StoredValue {
            ext: String::new(),
            id: "filter".to_string(),
            value: Value::Str("a\x00500".to_string()),
        }];
        let log = Arc::new(CapturingLog::default());
        let settings = with_store(Arc::clone(&store), Arc::clone(&log));

        settings
            .register(
                "openmpt",
                vec![schema(
                    "filter",
                    Setting::StrChoice {
                        value: "auto".to_string(),
                        choices: Vec::new(),
                    },
                )],
            )
            .expect("registration");

        assert_eq!(
            settings.get("openmpt", "", "filter"),
            Ok(Value::Str("auto".to_string()))
        );
        let lines = lock(&log.lines);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("interior NUL"), "{lines:?}");
    }

    #[test]
    fn enumeration_follows_registration_order_and_reports_current_values() {
        let settings = handle();
        settings
            .register("uade", vec![schema("filter", int(1))])
            .expect("registration");
        settings
            .register("hively", vec![schema("volume", int(64))])
            .expect("registration");
        settings
            .set("uade", "", "filter", Value::Int(3))
            .expect("write");

        assert_eq!(settings.reg_ids(), ["uade", "hively"]);
        assert_eq!(settings.schemas("uade")[0].setting, int(3));
        assert!(settings.schemas("nope").is_empty());
    }
}
