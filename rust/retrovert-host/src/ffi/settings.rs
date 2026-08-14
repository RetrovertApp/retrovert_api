//! Mirror of `retrovert/settings.h`.

use core::ffi::{c_char, c_int, c_void};

/// Values for the `widget_type` field shared by every settings descriptor.
pub const RVS_FLOAT_TYPE: u64 = 0x1000;
pub const RVS_INTEGER_TYPE: u64 = 0x1001;
pub const RVS_BOOL_TYPE: u64 = 0x1002;
pub const RVS_INTEGER_RANGE_TYPE: u64 = 0x1003;
pub const RVS_STRING_RANGE_TYPE: u64 = 0x1004;

/// Maximum number of settings accepted in one registration.
pub const RV_SETTINGS_COUNT_LIMIT: u64 = 1024;
/// Maximum number of choices accepted by one fixed-range setting.
pub const RVS_CHOICE_COUNT_LIMIT: u64 = 1024;

pub const RV_SETTINGS_API_VERSION: c_int = 1;

abi_enum! {
    /// Outcome of a settings registration or lookup.
    RVSettingsResult {
        Ok = 0,
        NotFound = 1,
        UnknownId = 2,
        DuplicatedId = 3,
        WrongType = 4,
        InvalidCount = 5,
    }
}

/// The header of every settings descriptor; the [`RVSetting`] members all start with it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSBase {
    pub widget_id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub widget_type: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSFloat {
    pub widget_id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub widget_type: u64,
    pub value: f32,
    pub start_range: f32,
    pub end_range: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSInteger {
    pub widget_id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub widget_type: u64,
    pub value: c_int,
    pub start_range: c_int,
    pub end_range: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSBool {
    pub widget_id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub widget_type: u64,
    pub value: bool,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSIntegerRangeValue {
    pub name: *const c_char,
    pub value: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSStringRangeValue {
    pub name: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSIntegerFixedRange {
    pub widget_id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub widget_type: u64,
    pub value: c_int,
    pub values: *mut RVSIntegerRangeValue,
    pub values_size: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSStringFixedRange {
    pub widget_id: *const c_char,
    pub name: *const c_char,
    pub desc: *const c_char,
    pub widget_type: u64,
    pub value: *const c_char,
    pub values: *mut RVSStringRangeValue,
    pub values_size: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSIntResult {
    pub result: u32,
    pub value: c_int,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSFloatResult {
    pub result: u32,
    pub value: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSStringResult {
    pub result: u32,
    pub value: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSBoolResult {
    pub result: u32,
    pub value: bool,
}

/// One entry of the array a plugin passes to [`RVSettings::reg`]. The active member is
/// identified by the `widget_type` of the [`RVSBase`] every member starts with.
#[repr(C)]
#[derive(Clone, Copy)]
pub union RVSetting {
    pub int_value: RVSInteger,
    pub float_value: RVSFloat,
    pub int_fixed_value: RVSIntegerFixedRange,
    pub string_fixed_value: RVSStringFixedRange,
    pub bool_value: RVSBool,
}

/// Settings registration and lookup the host exposes to plugins.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVSettings {
    pub private_data: *mut c_void,
    pub reg: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut RVSetting, u64) -> u32>,
    pub get_string: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> RVSStringResult,
    >,
    pub get_int: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> RVSIntResult,
    >,
    pub get_float: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> RVSFloatResult,
    >,
    pub get_bool: Option<
        unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            *const c_char,
            *const c_char,
        ) -> RVSBoolResult,
    >,
}
