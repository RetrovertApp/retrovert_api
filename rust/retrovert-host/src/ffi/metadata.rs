//! Mirror of `retrovert/metadata.h`.

use core::ffi::{c_char, c_int, c_void, CStr};

pub const RV_METADATA_API_VERSION: c_int = 1;

pub const RV_METADATA_TITLE_TAG: &CStr = c"title";
pub const RV_METADATA_SONGTYPE_TAG: &CStr = c"song_type";
pub const RV_METADATA_AUTHORINGTOOL_TAG: &CStr = c"authoring_tool";
pub const RV_METADATA_ARTIST_TAG: &CStr = c"artist";
pub const RV_METADATA_ALBUM_TAG: &CStr = c"album";
pub const RV_METADATA_DATE_TAG: &CStr = c"date";
pub const RV_METADATA_GENRE_TAG: &CStr = c"genre";
pub const RV_METADATA_MESSAGE_TAG: &CStr = c"message";
pub const RV_METADATA_LENGTH_TAG: &CStr = c"length";

/// Identifies the record a plugin's metadata pushes apply to.
pub type RVMetadataId = u64;

abi_enum! {
    /// Text encoding a plugin reports its metadata strings in.
    RVMetaEncoding {
        Utf8 = 0,
        ShiftJS2 = 1,
    }
}

abi_enum! {
    /// Why a metadata query failed. The doubled prefix is the name in the header.
    RVRVMetadataResult {
        KeyNotFound = 0,
        UnableToMakeQuery = 1,
    }
}

/// Metadata reporting the host exposes to plugins.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RVMetadata {
    pub private_data: *mut c_void,
    pub create_url: Option<unsafe extern "C" fn(*mut c_void, *const c_char) -> RVMetadataId>,
    pub set_tag:
        Option<unsafe extern "C" fn(*mut c_void, RVMetadataId, *const c_char, *const c_char)>,
    pub set_tag_f64: Option<unsafe extern "C" fn(*mut c_void, RVMetadataId, *const c_char, f64)>,
    pub add_subsong:
        Option<unsafe extern "C" fn(*mut c_void, RVMetadataId, u32, *const c_char, f32)>,
    pub add_sample: Option<unsafe extern "C" fn(*mut c_void, RVMetadataId, *const c_char)>,
    pub add_instrument: Option<unsafe extern "C" fn(*mut c_void, RVMetadataId, *const c_char)>,
}
