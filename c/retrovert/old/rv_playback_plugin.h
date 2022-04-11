#pragma once

#include <stdint.h>

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
extern "C" {
#endif

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RV_PLAYBACK_PLUGIN_API_VERSION 1

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

enum RVProbeResult {
    RVProbeResult_Supported,
    RVProbeResult_Unsupported,
    RVProbeResult_Unsure,
};

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

struct RVServiceAPI;
struct RVSettingsAPI;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

enum {
    RVOutputType_u8 = 1,
    RVOutputType_s16 = 2,
    RVOutputType_s24 = 3,  // Tightly packed. 3 bytes per sample.
    RVOutputType_s32 = 4,
    RVOutputType_f32 = 5,
};

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVReadInfo {
    uint32_t sample_rate;
    uint16_t sample_count;
    uint8_t channel_count;
    uint8_t output_format;
} RVReadInfo;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef enum RVSettingsUpdate {
    // Return if setting doesn't require restart
    RVSettingsUpdate_Default,
    // Return of setting require song to be restarted
    RVSettingsUpdate_RequireSongRestart,
} RVSettingsUpdate;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVPlaybackPlugin {
    // API Version of the plugin, always set this to RV_PLAYBACK_PLUGIN_API_VERSION
    uint64_t api_version;

    // Name of the plugin. This name should be unique or it may fail to load if there is a collision.
    const char* name;

    // Scematic version of the plugin. If the version doesn't follow the rules of SemVersion it may fail to load.
    const char* version;

    // Scematic version of library being used. Useful if you only update lib x from 0.1 to 0.2 but no other changes to
    // plugin In case the plugin doesn't use any external library this can be set to "" or NULL
    const char* library_version;

    // Ask the plugin if it can play some data. The plugin has to determine from the header if it supports the
    // file or not. The input data is at least 2048 bytes but can be less if the the total file is smaller.
    //
    // Notice that no user data is provided with this as the plugin instance hasn't actually been created
    // The plugin must support to parse this data without custom data being setup.
    //
    // It's encouraged that plugins detect the song-type based on metadata, but the filename is included
    // in case that is the only option to detect support for the file type
    enum RVProbeResult (*probe_can_play)(const uint8_t* data, uint32_t data_size, const char* filename,
                                         uint64_t total_size);

    // Returns a comma separated list of supported extensions
    const char* (*supported_extensions)();

    // Create an instace of the plugin. The user is expected to return an instance data poniter
    // that will get passed to the other callback functions.
    // Also a services pointer is passed down to create function that has various
    // services that the plugin can use (such as FileIO, metadata registration and more)
    // see the RVServicesAPI documentation for mor info
    void* (*create)(const RVServiceAPI* services);

    // Destroy the instance of the plugin. It's expected that the user will free the user_data pointer at
    // this point as it won't be used anymore.
    int (*destroy)(void* user_data);

    // TODO
    void (*event)(void* user_data, const unsigned char* data, int len);

    // Opens a buffer to be ready for playback. Buffer may be a file/archived/file or a file or a network resource.
    // Use the RVFileAPI that can be optained from services to load the data
    int (*open)(void* user_data, const char* url, int subsong, const struct RVSettingsAPI* settings);

    // Closes the file buffer that was opend in open. Notice that the plugin isn't detroyed at this but but is
    // here for closing an open file/stream/etc
    int (*close)(void* user_data);

    // Called when RV is requesting sample output from the the plugin.
    // The plugin is allowed to return as many samples as it want's as long as it doesn't go above max sample count
    RVReadInfo (*read_data)(void* user_data, void* dest, uint32_t max_output_bytes, uint32_t native_sample_rate);

    // Called requesting a new location in the data
    int (*seek)(void* user_data, int ms);

    // Called to see if the plugin can provide some metadata given an url
    int (*metadata)(const char* url, const struct RVServiceAPI* services);

    // Called once for each plugin. This allows the plugin to setup an instance of the logging api
    void (*static_init)(struct RVLogAPI* log, const struct RVServiceAPI* services);

    // Called when the user has changed some settings
    RVSettingsUpdate (*settings_updated)(void* user_data, const struct RVSettingsAPI* settings);
} RVPlaybackPlugin;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _WIN32
#define RV_EXPORT __declspec(dllexport)
#else
#define RV_EXPORT
#endif

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
}
#endif
