#pragma once

#include <stdint.h>

#ifdef _cplusplus
extern "C" {
#endif

struct RVMetadataAPIPrivData;
typedef uint64_t RVMetadataId;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

enum RVMetaEncoding {
    RVMetaEncoding_UTF8,
    RVMetaEncoding_ShiftJS2,
};

enum RVMetadataResult {
    RVMetadataResult_KeyNotFound = 0,
    RVMetadataResult_UnableToMakeQuery = -1,
};

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RV_METADATA_API_VERSION 1

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RVMetadata_TitleTag "title"
#define RVMetadata_SongTypeTag "song_type"
#define RVMetadata_AuthoringToolTag "authoring_tool"
#define RVMetadata_ArtistTag "artist"
#define RVMetadata_AlbumTag "album"
#define RVMetadata_DateTag "date"
#define RVMetadata_GenreTag "genre"
#define RVMetadata_MessageTag "message"
#define RVMetadata_LengthTag "length"
#define RVMetadata_SamplesTag "sample_"
#define RVMetadata_InstrumentsTag "instrument_"

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVMetadataAPI {
    struct RVMetadataAPIPrivData* priv_data;

    RVMetadataId (*create_url)(struct RVMetadataAPIPrivData* priv_data, const char* url);
    void (*set_tag)(struct RVMetadataAPIPrivData* priv_data, RVMetadataId id, const char* tag, const char* data);
    void (*set_tag_f64)(struct RVMetadataAPIPrivData* priv_data, RVMetadataId id, const char* tag, double d);
    void (*add_subsong)(struct RVMetadataAPIPrivData* priv_data, RVMetadataId parent_id, int index, const char* name,
                        float length);

    void (*add_sample)(struct RVMetadataAPIPrivData* priv_data, RVMetadataId parent_id, const char* text);
    void (*add_instrument)(struct RVMetadataAPIPrivData* priv_data, RVMetadataId parent_id, const char* text);
    int (*begin_get_all)(struct RVMetadataAPIPrivData* priv_data, const char* url);
    void (*end_get_all)(struct RVMetadataAPIPrivData* priv_data);
    int (*get_all_entry)(struct RVMetadataAPIPrivData* priv_data, int entry, const char** name, const char** data,
                         int* len_name, int* len_data);
    int (*get_all_sample)(struct RVMetadataAPIPrivData* priv_data, int entry, const char** text, int* text_len);
    int (*get_all_instrument)(struct RVMetadataAPIPrivData* priv_data, int entry, const char** text, int* text_len);

} RVMetadataAPI;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RVMetadata_create_url(api, url) api->create_url(api->priv_data, url)
#define RVMetadata_set_tag(api, id, tag, data) api->set_tag(api->priv_data, id, tag, data)
#define RVMetadata_set_tag_f64(api, id, tag, data) api->set_tag_f64(api->priv_data, id, tag, data)

#define RVMetadata_add_subsong(api, url, index, name, len) api->add_subsong(api->priv_data, url, index, name, len)
#define RVMetadata_add_sample(api, url, text) api->add_sample(api->priv_data, url, text)
#define RVMetadata_add_instrument(api, url, text) api->add_instrument(api->priv_data, url, text)

#define RVMetadata_begin_get_all(api, url) api->begin_get_all(api->priv_data, url)
#define RVMetadata_end_all(api) api->begin_get_all(api->priv_data)
#define RVMetadata_get_all_entry(api, index, name, data, name_len, data_len) \
    api->get_all_entry(api->priv_data, index, name, data, name_len, data_len)
#define RVMetadata_get_all_sample(api, index, text, text_len) api->get_all_sample(api->priv_data, index, text, text_len)
#define RVMetadata_get_all_instrument(api, index, text, text_len) \
    api->get_all_instrument(api->priv_data, index, text, text_len)

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
}
#endif
