//! The metadata service: what a plugin reports about the song it opened.
//!
//! The one service with no host trait. Plugins push tags, subsongs and names incrementally
//! and never say when they are done, so the crate accumulates them into a record it owns and
//! the consumer takes that record once the call which produced it has returned.
//!
//! Everything a plugin pushes is hostile input: arbitrary bytes, unbounded counts. The flags
//! on the record describe what the plugin sent, not what survived, so a string is booked as
//! cut or invalid even when the push it belonged to was then dropped.

use core::ffi::{c_char, c_void};
use std::sync::{Arc, Mutex};

use crate::ffi::metadata::RVMetadataId;
use crate::service::{context, guard, lock};

/// Longest tag key kept, in bytes.
pub const TAG_KEY_LIMIT: usize = 64;
/// Longest tag value kept, in bytes.
pub const TAG_VALUE_LIMIT: usize = 4 * 1024;
/// Most tags kept on one record.
pub const TAG_COUNT_LIMIT: usize = 32;
/// Most subsongs kept on one record.
pub const SUBSONG_COUNT_LIMIT: usize = 1024;
/// Longest subsong, sample or instrument name kept, in bytes.
pub const NAME_LIMIT: usize = 128;
/// Most sample names, and separately most instrument names, kept on one record.
pub const NAME_COUNT_LIMIT: usize = 256;
/// Longest url kept, in bytes.
pub const URL_LIMIT: usize = 64 * 1024;

/// A string a plugin pushed. `raw` holds the original bytes when they were not UTF-8, so a
/// consumer can still recover what the plugin meant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataText {
    pub text: String,
    pub raw: Option<Vec<u8>>,
}

/// What a tag carries: plugins push text and numbers through separate ABI calls.
#[derive(Debug, Clone, PartialEq)]
pub enum MetadataValue {
    Text(MetadataText),
    Number(f64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataTag {
    pub key: String,
    /// The key's original bytes, when they were not UTF-8.
    pub raw_key: Option<Vec<u8>>,
    pub value: MetadataValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataSubsong {
    pub index: u32,
    pub name: MetadataText,
    /// `None` when the plugin reported no usable length.
    pub length_seconds: Option<f32>,
}

/// Whether the subsong list is the plugin's or the single one assumed for a plugin that
/// reported none.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SubsongCount {
    #[default]
    Reported,
    Assumed,
}

/// What the caps cut: one flag per kind of string, one per collection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetadataTruncation {
    pub url: bool,
    pub tag_key: bool,
    pub tag_value: bool,
    /// A subsong, sample or instrument name was cut to [`NAME_LIMIT`].
    pub name: bool,
    pub tags: bool,
    pub subsongs: bool,
    pub samples: bool,
    pub instruments: bool,
}

/// The store's identity for one record, minted by the store and never by a plugin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordId(pub u64);

/// Everything the plugins reported about one song.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackMetadata {
    pub id: RecordId,
    /// The url the plugin named, empty until it calls `create_url`.
    pub url: MetadataText,
    pub tags: Vec<MetadataTag>,
    pub subsongs: Vec<MetadataSubsong>,
    pub samples: Vec<MetadataText>,
    pub instruments: Vec<MetadataText>,
    pub subsong_count: SubsongCount,
    /// A string arrived that was not UTF-8; the replacement text carries `raw` beside it.
    pub invalid_utf8: bool,
    /// A tag arrived with no key, and was dropped: nothing could look it up.
    pub empty_tag_key: bool,
    /// A number arrived as NaN or an infinity — a tag value, or a subsong length — and was
    /// not used.
    pub non_finite_number: bool,
    /// A push named a record this store never minted. It landed here anyway — there is only
    /// ever one — but the plugin is not using the id `create_url` gave it.
    pub foreign_record_id: bool,
    pub truncation: MetadataTruncation,
}

impl TrackMetadata {
    fn new(id: RecordId) -> Self {
        Self {
            id,
            ..Self::default()
        }
    }

    fn note_id(&mut self, id: RVMetadataId) {
        self.foreign_record_id |= id != self.id.0;
    }

    fn accept(
        &mut self,
        decoded: Decoded,
        cut: fn(&mut MetadataTruncation) -> &mut bool,
    ) -> MetadataText {
        self.invalid_utf8 |= decoded.invalid;
        if decoded.truncated {
            *cut(&mut self.truncation) = true;
        }
        decoded.text
    }

    fn names(&mut self, kind: NameKind) -> (&mut Vec<MetadataText>, &mut bool) {
        match kind {
            NameKind::Sample => (&mut self.samples, &mut self.truncation.samples),
            NameKind::Instrument => (&mut self.instruments, &mut self.truncation.instruments),
        }
    }

    /// A plugin that named no subsong still has one to play.
    fn finish(mut self) -> Self {
        if self.subsongs.is_empty() {
            self.subsong_count = SubsongCount::Assumed;
            self.subsongs.push(MetadataSubsong {
                index: 0,
                name: MetadataText::default(),
                length_seconds: None,
            });
        }
        self
    }
}

/// The record plugins are pushing into, and the consumer's way to take it.
///
/// Cloning shares one record: the plugin pushes from the decode thread while the consumer
/// takes from its own.
#[derive(Clone)]
pub struct MetadataHandle {
    record: Arc<Mutex<TrackMetadata>>,
}

impl MetadataHandle {
    pub(super) fn new() -> Self {
        Self {
            record: Arc::new(Mutex::new(TrackMetadata::new(RecordId(1)))),
        }
    }

    /// Takes what has accumulated and starts an empty record under a fresh id. Call it once
    /// the plugin call that produced the pushes has returned.
    pub fn take(&self) -> TrackMetadata {
        let mut record = lock(&self.record);
        let next = TrackMetadata::new(RecordId(record.id.0.wrapping_add(1)));
        core::mem::replace(&mut *record, next).finish()
    }
}

struct Decoded {
    text: MetadataText,
    truncated: bool,
    invalid: bool,
}

/// Reads at most `limit` bytes of a plugin string.
///
/// The cap is read one byte long so a string that exactly fills it is not reported as cut.
///
/// # Safety
///
/// `value` must be null, or a NUL-terminated string readable through its terminator.
unsafe fn bounded_bytes<'a>(value: *const c_char, limit: usize) -> (&'a [u8], bool) {
    if value.is_null() {
        return (&[], false);
    }
    // SAFETY: the caller guarantees a terminated string, so the scan stops inside it.
    let length = unsafe { libc::strnlen(value, limit + 1) };
    // SAFETY: `strnlen` found that many readable bytes before the terminator.
    let bytes = unsafe { core::slice::from_raw_parts(value.cast::<u8>(), length.min(limit)) };
    (bytes, length > limit)
}

/// Cuts an over-long string on a character boundary rather than mid-codepoint.
///
/// # Safety
///
/// `value` must be null, or a NUL-terminated string readable through its terminator.
unsafe fn decode(value: *const c_char, limit: usize) -> Decoded {
    // SAFETY: the caller guarantees the string.
    let (mut bytes, truncated) = unsafe { bounded_bytes(value, limit) };

    // A cap landing inside a character drops the partial one: that is the cut being
    // reported, not a string worth calling invalid.
    if truncated {
        if let Err(error) = core::str::from_utf8(bytes) {
            if error.error_len().is_none() {
                bytes = &bytes[..error.valid_up_to()];
            }
        }
    }

    match core::str::from_utf8(bytes) {
        Ok(text) => Decoded {
            text: MetadataText {
                text: text.to_owned(),
                raw: None,
            },
            truncated,
            invalid: false,
        },
        Err(_) => Decoded {
            text: MetadataText {
                text: String::from_utf8_lossy(bytes).into_owned(),
                raw: Some(bytes.to_vec()),
            },
            truncated,
            invalid: true,
        },
    }
}

#[derive(Clone, Copy)]
enum NameKind {
    Sample,
    Instrument,
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `url`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn create_url(
    private_data: *mut c_void,
    url: *const c_char,
) -> RVMetadataId {
    guard(0, || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, decoded) = unsafe { (context(private_data), decode(url, URL_LIMIT)) };
        let mut record = lock(&ctx.metadata.record);
        let url = record.accept(decoded, |cut| &mut cut.url);
        // The ABI lets a plugin mint a record per url; every shipped plugin names only the
        // url it was opened with, so the store keeps the first and re-answers its id.
        if record.url.text.is_empty() {
            record.url = url;
        }
        record.id.0
    })
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `tag` and
/// `data` must each be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn set_tag(
    private_data: *mut c_void,
    id: RVMetadataId,
    tag: *const c_char,
    data: *const c_char,
) {
    guard((), || {
        // SAFETY: the caller guarantees the context pointer and both strings.
        let (ctx, key, value) = unsafe {
            (
                context(private_data),
                decode(tag, TAG_KEY_LIMIT),
                decode(data, TAG_VALUE_LIMIT),
            )
        };
        let mut record = lock(&ctx.metadata.record);
        record.note_id(id);
        let key = record.accept(key, |cut| &mut cut.tag_key);
        let value = record.accept(value, |cut| &mut cut.tag_value);
        push_tag(&mut record, key, MetadataValue::Text(value));
    })
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `tag`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn set_tag_f64(
    private_data: *mut c_void,
    id: RVMetadataId,
    tag: *const c_char,
    data: f64,
) {
    guard((), || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, key) = unsafe { (context(private_data), decode(tag, TAG_KEY_LIMIT)) };
        let mut record = lock(&ctx.metadata.record);
        record.note_id(id);
        let key = record.accept(key, |cut| &mut cut.tag_key);
        push_tag(&mut record, key, MetadataValue::Number(data));
    })
}

/// A tag with no key cannot be looked up and a number that is not finite cannot be used.
/// Both are reported before either is allowed to drop the tag, so a push that is wrong twice
/// says so twice.
fn push_tag(record: &mut TrackMetadata, key: MetadataText, value: MetadataValue) {
    let keyless = key.text.is_empty();
    let unusable = matches!(value, MetadataValue::Number(number) if !number.is_finite());
    record.empty_tag_key |= keyless;
    record.non_finite_number |= unusable;
    if keyless || unusable {
        return;
    }
    if record.tags.len() >= TAG_COUNT_LIMIT {
        record.truncation.tags = true;
        return;
    }
    record.tags.push(MetadataTag {
        key: key.text,
        raw_key: key.raw,
        value,
    });
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `name`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn add_subsong(
    private_data: *mut c_void,
    parent_id: RVMetadataId,
    index: u32,
    name: *const c_char,
    length: f32,
) {
    guard((), || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, name) = unsafe { (context(private_data), decode(name, NAME_LIMIT)) };
        let mut record = lock(&ctx.metadata.record);
        record.note_id(parent_id);
        let name = record.accept(name, |cut| &mut cut.name);
        record.non_finite_number |= !length.is_finite();
        if record.subsongs.len() >= SUBSONG_COUNT_LIMIT {
            record.truncation.subsongs = true;
            return;
        }
        record.subsongs.push(MetadataSubsong {
            index,
            name,
            length_seconds: (length.is_finite() && length > 0.0).then_some(length),
        });
    })
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `text`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn add_sample(
    private_data: *mut c_void,
    parent_id: RVMetadataId,
    text: *const c_char,
) {
    // SAFETY: the caller's guarantees are this function's own.
    unsafe { add_name(private_data, parent_id, text, NameKind::Sample) }
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `text`
/// must be null or a NUL-terminated string readable for its whole length.
pub(super) unsafe extern "C" fn add_instrument(
    private_data: *mut c_void,
    parent_id: RVMetadataId,
    text: *const c_char,
) {
    // SAFETY: the caller's guarantees are this function's own.
    unsafe { add_name(private_data, parent_id, text, NameKind::Instrument) }
}

/// # Safety
///
/// `private_data` must be the context pointer installed by `ServiceHost::new`, and `text`
/// must be null or a NUL-terminated string readable for its whole length.
unsafe fn add_name(
    private_data: *mut c_void,
    parent_id: RVMetadataId,
    text: *const c_char,
    kind: NameKind,
) {
    guard((), || {
        // SAFETY: the caller guarantees the context pointer and the string.
        let (ctx, decoded) = unsafe { (context(private_data), decode(text, NAME_LIMIT)) };
        let mut record = lock(&ctx.metadata.record);
        record.note_id(parent_id);
        let text = record.accept(decoded, |cut| &mut cut.name);

        let (list, cut) = record.names(kind);
        if list.len() >= NAME_COUNT_LIMIT {
            *cut = true;
            return;
        }
        list.push(text);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use core::ptr;
    use std::ffi::CString;

    use crate::ffi::metadata::{RVMetadata, RV_METADATA_API_VERSION};
    use crate::service::ServiceHost;

    fn vtable(host: &ServiceHost) -> &RVMetadata {
        let service = host.service();
        // SAFETY: the getter answers the version it was built for with a live vtable.
        unsafe {
            &*(service.get_metadata.expect("get_metadata"))(
                service.private_data,
                RV_METADATA_API_VERSION,
            )
        }
    }

    /// Drives the vtable the way a plugin does: one url, then pushes under its id.
    struct Plugin<'a> {
        vtable: &'a RVMetadata,
        id: RVMetadataId,
    }

    impl<'a> Plugin<'a> {
        fn open(host: &'a ServiceHost, url: &str) -> Self {
            let vtable = vtable(host);
            let url = CString::new(url).expect("url");
            // SAFETY: `url` is live and NUL-terminated for the call.
            let id = unsafe {
                (vtable.create_url.expect("create_url"))(vtable.private_data, url.as_ptr())
            };
            Self { vtable, id }
        }

        fn anonymous(host: &'a ServiceHost) -> Self {
            Self {
                vtable: vtable(host),
                id: 0,
            }
        }

        fn tag(&self, key: &str, value: &str) {
            let (key, value) = (
                CString::new(key).expect("key"),
                CString::new(value).expect("value"),
            );
            // SAFETY: both strings are live and NUL-terminated for the call.
            unsafe {
                (self.vtable.set_tag.expect("set_tag"))(
                    self.vtable.private_data,
                    self.id,
                    key.as_ptr(),
                    value.as_ptr(),
                )
            }
        }

        fn number(&self, key: &str, value: f64) {
            let key = CString::new(key).expect("key");
            // SAFETY: `key` is live and NUL-terminated for the call.
            unsafe {
                (self.vtable.set_tag_f64.expect("set_tag_f64"))(
                    self.vtable.private_data,
                    self.id,
                    key.as_ptr(),
                    value,
                )
            }
        }

        fn subsong(&self, index: u32, name: &str, length: f32) {
            let name = CString::new(name).expect("name");
            // SAFETY: `name` is live and NUL-terminated for the call.
            unsafe {
                (self.vtable.add_subsong.expect("add_subsong"))(
                    self.vtable.private_data,
                    self.id,
                    index,
                    name.as_ptr(),
                    length,
                )
            }
        }

        fn sample(&self, text: &str) {
            let text = CString::new(text).expect("text");
            // SAFETY: `text` is live and NUL-terminated for the call.
            unsafe {
                (self.vtable.add_sample.expect("add_sample"))(
                    self.vtable.private_data,
                    self.id,
                    text.as_ptr(),
                )
            }
        }

        fn instrument(&self, text: &str) {
            let text = CString::new(text).expect("text");
            // SAFETY: `text` is live and NUL-terminated for the call.
            unsafe {
                (self.vtable.add_instrument.expect("add_instrument"))(
                    self.vtable.private_data,
                    self.id,
                    text.as_ptr(),
                )
            }
        }
    }

    fn text(value: &str) -> MetadataText {
        MetadataText {
            text: value.to_owned(),
            raw: None,
        }
    }

    #[test]
    fn a_whole_record_arrives_through_the_vtable_and_is_taken_once() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "/songs/enigma.mod");
        plugin.tag("title", "Enigma");
        plugin.number("length", 383.5);
        plugin.subsong(0, "intro", 12.0);
        plugin.subsong(1, "main", 0.0);
        plugin.sample("bassdrum");
        plugin.instrument("lead");

        let record = host.metadata().take();
        assert_eq!(record.id, RecordId(1));
        assert_eq!(plugin.id, record.id.0);
        assert_eq!(record.url, text("/songs/enigma.mod"));
        assert_eq!(
            record.tags,
            [
                MetadataTag {
                    key: "title".to_owned(),
                    raw_key: None,
                    value: MetadataValue::Text(text("Enigma")),
                },
                MetadataTag {
                    key: "length".to_owned(),
                    raw_key: None,
                    value: MetadataValue::Number(383.5),
                },
            ]
        );
        assert_eq!(
            record.subsongs,
            [
                MetadataSubsong {
                    index: 0,
                    name: text("intro"),
                    length_seconds: Some(12.0),
                },
                // A length of zero is the plugin saying it does not know.
                MetadataSubsong {
                    index: 1,
                    name: text("main"),
                    length_seconds: None,
                },
            ]
        );
        assert_eq!(record.subsong_count, SubsongCount::Reported);
        assert_eq!(record.samples, [text("bassdrum")]);
        assert_eq!(record.instruments, [text("lead")]);
        assert_eq!(record.truncation, MetadataTruncation::default());
        assert!(!record.invalid_utf8);
        assert!(!record.foreign_record_id);

        // Taking the record leaves an empty one behind, under a new identity.
        let next = host.metadata().take();
        assert_eq!(next.id, RecordId(2));
        assert!(next.tags.is_empty());
        assert_eq!(next.url, MetadataText::default());
    }

    #[test]
    fn a_plugin_that_names_no_subsong_still_gets_one() {
        let host = ServiceHost::default();
        Plugin::open(&host, "song.sid").tag("title", "Commando");

        let record = host.metadata().take();
        assert_eq!(record.subsong_count, SubsongCount::Assumed);
        assert_eq!(
            record.subsongs,
            [MetadataSubsong {
                index: 0,
                name: MetadataText::default(),
                length_seconds: None,
            }]
        );
    }

    #[test]
    fn every_collection_stops_at_its_cap_without_losing_the_record() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        for index in 0..TAG_COUNT_LIMIT + 1 {
            plugin.tag(&format!("key-{index}"), "value");
        }
        for index in 0..SUBSONG_COUNT_LIMIT as u32 + 1 {
            plugin.subsong(index, "", 0.0);
        }
        for _ in 0..NAME_COUNT_LIMIT + 1 {
            plugin.sample("sample");
            plugin.instrument("instrument");
        }

        let record = host.metadata().take();
        assert_eq!(record.tags.len(), TAG_COUNT_LIMIT);
        assert_eq!(record.subsongs.len(), SUBSONG_COUNT_LIMIT);
        assert_eq!(record.samples.len(), NAME_COUNT_LIMIT);
        assert_eq!(record.instruments.len(), NAME_COUNT_LIMIT);
        assert_eq!(
            record.truncation,
            MetadataTruncation {
                tags: true,
                subsongs: true,
                samples: true,
                instruments: true,
                ..MetadataTruncation::default()
            }
        );
        // The first pushes are the ones kept.
        assert_eq!(record.tags[0].key, "key-0");
        assert_eq!(record.subsongs.last().expect("subsong").index, 1023);
    }

    #[test]
    fn numeric_tags_stop_at_the_same_cap_as_text_ones() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        for index in 0..TAG_COUNT_LIMIT + 1 {
            plugin.number(&format!("key-{index}"), index as f64);
        }

        let record = host.metadata().take();
        assert_eq!(record.tags.len(), TAG_COUNT_LIMIT);
        assert!(record.truncation.tags);
    }

    #[test]
    fn a_string_that_exactly_fills_its_cap_is_not_reported_as_cut() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        plugin.tag(&"k".repeat(TAG_KEY_LIMIT), &"v".repeat(TAG_VALUE_LIMIT));
        plugin.sample(&"s".repeat(NAME_LIMIT));

        let record = host.metadata().take();
        assert_eq!(record.tags[0].key, "k".repeat(TAG_KEY_LIMIT));
        assert_eq!(record.samples[0].text, "s".repeat(NAME_LIMIT));
        assert_eq!(record.truncation, MetadataTruncation::default());
    }

    #[test]
    fn a_string_past_its_cap_is_cut_to_the_cap_and_reported() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        plugin.tag(
            &"k".repeat(TAG_KEY_LIMIT + 1),
            &"v".repeat(TAG_VALUE_LIMIT + 1),
        );
        plugin.subsong(0, &"n".repeat(NAME_LIMIT + 1), 1.0);

        let record = host.metadata().take();
        assert_eq!(record.tags[0].key, "k".repeat(TAG_KEY_LIMIT));
        assert_eq!(
            record.tags[0].value,
            MetadataValue::Text(text(&"v".repeat(TAG_VALUE_LIMIT)))
        );
        assert_eq!(record.subsongs[0].name, text(&"n".repeat(NAME_LIMIT)));
        assert_eq!(
            record.truncation,
            MetadataTruncation {
                tag_key: true,
                tag_value: true,
                name: true,
                ..MetadataTruncation::default()
            }
        );
        assert!(!record.invalid_utf8);
    }

    #[test]
    fn a_cap_landing_inside_a_character_drops_it_rather_than_calling_the_string_invalid() {
        let host = ServiceHost::default();
        // The last 'é' straddles the cap: two bytes, only the first of which fits.
        let name = format!("{}é", "a".repeat(NAME_LIMIT - 1));
        Plugin::open(&host, "song.mod").sample(&name);

        let record = host.metadata().take();
        assert_eq!(record.samples[0], text(&"a".repeat(NAME_LIMIT - 1)));
        assert!(record.truncation.name);
        assert!(!record.invalid_utf8);
    }

    #[test]
    fn bytes_that_are_not_utf8_are_replaced_flagged_and_kept_raw() {
        let host = ServiceHost::default();
        let vtable = vtable(&host);
        let key = CString::new(vec![b't', 0xff]).expect("key");
        let value = CString::new(vec![b'a', 0xfe, b'b']).expect("value");

        // SAFETY: both strings are live and NUL-terminated for the call.
        unsafe {
            (vtable.set_tag.expect("set_tag"))(vtable.private_data, 0, key.as_ptr(), value.as_ptr())
        };

        let record = host.metadata().take();
        assert!(record.invalid_utf8);
        assert_eq!(record.tags[0].key, "t\u{fffd}");
        assert_eq!(record.tags[0].raw_key, Some(vec![b't', 0xff]));
        assert_eq!(
            record.tags[0].value,
            MetadataValue::Text(MetadataText {
                text: "a\u{fffd}b".to_owned(),
                raw: Some(vec![b'a', 0xfe, b'b']),
            })
        );
        assert_eq!(record.truncation, MetadataTruncation::default());
    }

    #[test]
    fn null_strings_from_a_plugin_read_as_empty_and_a_keyless_tag_is_dropped() {
        let host = ServiceHost::default();
        let vtable = vtable(&host);

        // SAFETY: null is what a plugin passing an absent string sends.
        unsafe {
            (vtable.create_url.expect("create_url"))(vtable.private_data, ptr::null());
            (vtable.set_tag.expect("set_tag"))(vtable.private_data, 1, ptr::null(), ptr::null());
            (vtable.set_tag_f64.expect("set_tag_f64"))(vtable.private_data, 1, ptr::null(), 1.0);
            (vtable.add_subsong.expect("add_subsong"))(vtable.private_data, 1, 3, ptr::null(), 0.0);
            (vtable.add_sample.expect("add_sample"))(vtable.private_data, 1, ptr::null());
            (vtable.add_instrument.expect("add_instrument"))(vtable.private_data, 1, ptr::null());
        }

        let record = host.metadata().take();
        assert_eq!(record.url, MetadataText::default());
        assert!(record.tags.is_empty());
        assert!(record.empty_tag_key);
        // A nameless subsong or sample is still a subsong or sample.
        assert_eq!(
            record.subsongs,
            [MetadataSubsong {
                index: 3,
                name: MetadataText::default(),
                length_seconds: None,
            }]
        );
        assert_eq!(record.samples, [MetadataText::default()]);
        assert_eq!(record.instruments, [MetadataText::default()]);
        assert!(!record.invalid_utf8);
        assert_eq!(record.truncation, MetadataTruncation::default());
    }

    #[test]
    fn an_empty_key_is_dropped_and_a_number_that_is_not_finite_is_too() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        plugin.tag("", "orphan");
        plugin.number("", 1.0);
        plugin.number("length", f64::NAN);
        plugin.number("gain", f64::INFINITY);
        plugin.number("real", 2.0);

        let record = host.metadata().take();
        assert_eq!(record.tags.len(), 1);
        assert_eq!(record.tags[0].key, "real");
        assert!(record.empty_tag_key);
        assert!(record.non_finite_number);
    }

    #[test]
    fn a_push_that_is_dropped_still_reports_what_it_carried() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        // A second url is not the record's, but it was still over the cap.
        Plugin::open(&host, &"u".repeat(URL_LIMIT + 1));

        for index in 0..TAG_COUNT_LIMIT {
            plugin.tag(&format!("key-{index}"), "value");
        }
        plugin.tag(&"k".repeat(TAG_KEY_LIMIT + 1), "past the cap");

        for _ in 0..NAME_COUNT_LIMIT {
            plugin.sample("sample");
        }
        plugin.sample(&"s".repeat(NAME_LIMIT + 1));

        for index in 0..SUBSONG_COUNT_LIMIT as u32 {
            plugin.subsong(index, "subsong", 1.0);
        }
        plugin.subsong(0, "", f32::NAN);

        let record = host.metadata().take();
        assert_eq!(record.url, text("song.mod"));
        assert_eq!(record.tags.len(), TAG_COUNT_LIMIT);
        assert_eq!(record.samples.len(), NAME_COUNT_LIMIT);
        assert_eq!(record.subsongs.len(), SUBSONG_COUNT_LIMIT);
        assert_eq!(
            record.truncation,
            MetadataTruncation {
                url: true,
                tag_key: true,
                name: true,
                tags: true,
                samples: true,
                subsongs: true,
                ..MetadataTruncation::default()
            }
        );
        assert!(record.non_finite_number);
    }

    #[test]
    fn a_tag_that_is_both_keyless_and_not_finite_reports_both() {
        let host = ServiceHost::default();
        Plugin::open(&host, "song.mod").number("", f64::NAN);

        let record = host.metadata().take();
        assert!(record.tags.is_empty());
        assert!(record.empty_tag_key);
        assert!(record.non_finite_number);
    }

    #[test]
    fn a_subsong_length_that_is_not_usable_is_dropped_and_only_a_wild_one_is_reported() {
        let host = ServiceHost::default();
        let plugin = Plugin::open(&host, "song.mod");
        plugin.subsong(0, "negative", -1.0);
        plugin.subsong(1, "unknown", 0.0);

        let record = host.metadata().take();
        assert!(record.subsongs.iter().all(|s| s.length_seconds.is_none()));
        // A length the plugin simply does not know is not a plugin misbehaving.
        assert!(!record.non_finite_number);

        for wild in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let host = ServiceHost::default();
            Plugin::open(&host, "song.mod").subsong(0, "wild", wild);

            let record = host.metadata().take();
            assert_eq!(record.subsongs[0].length_seconds, None, "{wild}");
            assert!(record.non_finite_number, "{wild}");
        }
    }

    #[test]
    fn a_string_that_is_both_past_its_cap_and_not_utf8_is_reported_as_both() {
        let host = ServiceHost::default();
        let vtable = vtable(&host);
        // The invalid byte sits well inside the cap, so the char-boundary trim cannot apply.
        let mut name = vec![b'a', 0xff];
        name.extend(std::iter::repeat_n(b'b', NAME_LIMIT));
        let name = CString::new(name).expect("name");

        // SAFETY: `name` is live and NUL-terminated for the call.
        unsafe { (vtable.add_sample.expect("add_sample"))(vtable.private_data, 1, name.as_ptr()) };

        let record = host.metadata().take();
        assert!(record.invalid_utf8);
        assert!(record.truncation.name);
        assert_eq!(
            record.samples[0].raw.as_ref().expect("raw").len(),
            NAME_LIMIT
        );
        assert_eq!(
            record.samples[0].text,
            format!("a\u{fffd}{}", "b".repeat(NAME_LIMIT - 2))
        );
    }

    #[test]
    fn a_push_under_an_id_the_store_never_minted_is_kept_and_reported() {
        let host = ServiceHost::default();
        Plugin::anonymous(&host).tag("title", "No url was ever created");

        let record = host.metadata().take();
        assert!(record.foreign_record_id);
        assert_eq!(record.tags.len(), 1);
    }

    #[test]
    fn the_first_url_a_plugin_names_is_the_records_and_later_ones_do_not_replace_it() {
        let host = ServiceHost::default();
        let first = Plugin::open(&host, "song.mod");
        let second = Plugin::open(&host, "song.mod.instruments");

        assert_eq!(first.id, second.id);
        assert_eq!(host.metadata().take().url, text("song.mod"));
    }

    #[test]
    fn a_url_past_its_cap_is_cut_and_reported() {
        let host = ServiceHost::default();
        Plugin::open(&host, &"u".repeat(URL_LIMIT + 1));

        let record = host.metadata().take();
        assert_eq!(record.url.text.len(), URL_LIMIT);
        assert!(record.truncation.url);
    }

    #[test]
    fn no_push_is_lost_while_the_consumer_takes_records_from_another_thread() {
        let host = ServiceHost::default();
        // Samples, and fewer of them than their cap, so a taker that stalls cannot overflow
        // one record and turn a legitimately dropped push into a false loss.
        const PUSHES: usize = NAME_COUNT_LIMIT - 1;

        let taker = host.metadata();
        let taking = std::thread::spawn(move || {
            let mut taken = Vec::new();
            for _ in 0..PUSHES {
                let record = taker.take();
                taken.push((record.id, record.samples.len()));
            }
            taken
        });

        let plugin = Plugin::anonymous(&host);
        for index in 0..PUSHES {
            plugin.sample(&format!("push-{index}"));
        }

        let taken = taking.join().expect("taking thread");
        let seen = taken.iter().map(|(_, samples)| samples).sum::<usize>()
            + host.metadata().take().samples.len();
        assert_eq!(seen, PUSHES);
        // Every record the consumer took had its own identity.
        let ids = taken.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    }

    #[test]
    fn a_push_still_lands_after_a_panic_poisoned_the_record() {
        let host = ServiceHost::default();
        let handle = host.metadata();
        let poisoning = std::thread::spawn(move || {
            let _held = lock(&handle.record);
            panic!("consumer blew up mid-read")
        });
        assert!(
            poisoning.join().is_err(),
            "the thread was supposed to panic"
        );

        Plugin::open(&host, "song.mod").tag("title", "Survived");

        let record = host.metadata().take();
        assert_eq!(record.url, text("song.mod"));
        assert_eq!(record.tags.len(), 1);
    }
}

/// The metadata service as a real C plugin reaches it: through the header's own macros.
#[cfg(all(test, unix))]
mod c_tests {
    use super::*;

    use core::ffi::c_int;

    use libloading::{Library, Symbol};

    use crate::c_fixture::compile;
    use crate::ffi::service::RVService;
    use crate::service::ServiceHost;

    const FIXTURE: &str = r#"
#include <stddef.h>

#include <retrovert/metadata.h>
#include <retrovert/service.h>

RV_PLUGIN_USE_METADATA_API();

int fixture_init(const RVService* services) {
    g_rv_metadata = RVService_get_metadata(services, RV_METADATA_API_VERSION);
    return g_rv_metadata ? 0 : -1;
}

void fixture_report(const char* url) {
    RVMetadataId id = rv_metadata_create_url(url);
    rv_metadata_set_tag(id, RV_METADATA_TITLE_TAG, "Cream of the Earth");
    rv_metadata_set_tag(id, RV_METADATA_ARTIST_TAG, "Jester");
    rv_metadata_set_tag_f64(id, RV_METADATA_LENGTH_TAG, 214.0);
    rv_metadata_add_subsong(id, 0, "part one", 107.0f);
    rv_metadata_add_subsong(id, 1, "part two", 107.0f);
    rv_metadata_add_sample(id, "hihat");
    rv_metadata_add_instrument(id, "bass");
}
"#;

    #[test]
    fn a_c_plugin_reports_a_whole_song_and_the_host_takes_the_record() {
        let dir = tempfile::tempdir().expect("temp dir");
        let library = compile(dir.path(), "metadata_fixture", FIXTURE);
        // SAFETY: the fixture was just built from the source above and runs no static
        // initializers of its own.
        let library = unsafe { Library::new(&library) }.expect("load fixture");

        let host = ServiceHost::default();
        // SAFETY: each symbol is looked up with the signature the fixture defines, and the
        // url passed lives across the call.
        unsafe {
            let init: Symbol<unsafe extern "C" fn(*const RVService) -> c_int> =
                library.get(b"fixture_init").expect("fixture_init");
            assert_eq!(init(host.service()), 0, "no metadata service");

            let report: Symbol<unsafe extern "C" fn(*const c_char)> =
                library.get(b"fixture_report").expect("fixture_report");
            report(c"/songs/cream.mod".as_ptr());
        }

        let record = host.metadata().take();
        assert_eq!(record.url.text, "/songs/cream.mod");
        assert_eq!(
            record
                .tags
                .iter()
                .map(|tag| tag.key.as_str())
                .collect::<Vec<_>>(),
            ["title", "artist", "length"]
        );
        assert_eq!(record.tags[2].value, MetadataValue::Number(214.0));
        assert_eq!(record.subsongs.len(), 2);
        assert_eq!(record.subsongs[1].name.text, "part two");
        assert_eq!(record.subsong_count, SubsongCount::Reported);
        assert_eq!(record.samples[0].text, "hihat");
        assert_eq!(record.instruments[0].text, "bass");
        // The plugin used the id it was given throughout.
        assert!(!record.foreign_record_id);
    }
}
