#pragma once

#ifdef _cplusplus
extern "C" {
#endif

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

struct RVServicePrivData;

struct RVLogAPI;
struct RVSettingsAPI;
struct RVMetadataAPI;
struct RVMessageAPI;
struct RVSettingsAPI;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVServiceAPI {
    RVServicePrivData* private_data;
    const struct RVLogAPI* (*get_log_api)(struct RVServicePrivData* private_data, int api_version);
    const struct RVIoAPI* (*get_io_api)(struct RVServicePrivData* private_data, int api_version);
    const struct RVMetadataAPI* (*get_metadata_api)(struct RVServicePrivData* private_data, int api_version);
    const struct RVMessageAPI* (*get_message_api)(struct RVServicePrivData* private_data, int api_version);
    const struct RVSettingsAPI* (*get_settings_api)(struct RVServicePrivData* private_data, int api_version);
} RVServiceAPI;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RVServiceAPI_get_log_api(api, version) api->get_log_api(api->private_data, version)
#define RVServiceAPI_get_io_api(api, version) api->get_io_api(api->private_data, version)
#define RVServiceAPI_get_metadata_api(api, version) api->get_metadata_api(api->private_data, version)
#define RVServiceAPI_get_message_api(api, version) api->get_message_api(api->private_data, version)
#define RVServiceAPI_get_settings_api(api, version) api->get_settings_api(api->private_data, version)

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
}
#endif
