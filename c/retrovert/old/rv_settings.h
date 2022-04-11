#pragma once

#include <stdbool.h>

#ifdef _cplusplus
extern "C" {
#endif

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RVS_FLOAT_TYPE 0x1000
#define RVS_INTEGER_TYPE 0x1001
#define RVS_BOOL_TYPE 0x1002
#define RVS_INTEGER_RANGE_TYPE 0x1003
#define RVS_STRING_RANGE_TYPE 0x1004

#define RV_SETTINGS_API_VERSION 1

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef enum RVSettingResult {
    RVSettingsResult_Ok = 0,
    RVSettingsResult_SettingNotFound = 1,
    RVSettingsResult_KeyNotFound = 2,
    RVSettingsResult_InvalidType = 3,
} RVSettingResult;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSBase {
    const char* widget_id;
    const char* name;
    const char* desc;
    int widget_type;
} RVSBase;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSFloat {
    RVSBase base;
    float value;
    float start_range;
    float end_range;
} RVSFloat;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSInteger {
    RVSBase base;
    int value;
    int start_range;
    int end_range;
} RVSInteger;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSBool {
    RVSBase base;
    bool value;
} RVSBool;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSIntegerRangeValue {
    const char* name;
    int value;
} RVSIntegerRangeValue;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSStringRangeValue {
    const char* name;
    const char* value;
} RVSStringRangeValue;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
// start of this layout has to match RVSInteger

typedef struct RVSIntegerFixedRange {
    RVSBase base;
    int value;
    RVSIntegerRangeValue* values;
    int values_count;
} RVSIntegerFixedRange;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct StringFixedRange {
    RVSBase base;
    const char* value;
    RVSStringRangeValue* values;
    int values_count;
} RVSStringFixedRange;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef union RVSSetting {
    RVSInteger int_value;
    RVSFloat float_value;
    RVSIntegerFixedRange int_fixed_value;
    RVSStringFixedRange string_fixed_value;
    RVSBool bool_value;
} RVSSetting;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifndef rv_sizeof_array
#define rv_sizeof_array(x) sizeof(x) / sizeof(x[0])
#endif

#define RVSIntValue(id, name, desc, value)                             \
    {                                                                  \
        .int_value = { id, name, desc, RVS_INTEGER_TYPE, value, 0, 0 } \
    }
#define RVSFloatValue(id, name, desc, value)                                 \
    {                                                                        \
        .float_value = { id, name, desc, RVS_FLOAT_TYPE, value, 0.0f, 0.0f } \
    }
#define RVSFloatValue_Range(id, name, desc, value, start, end)               \
    {                                                                        \
        .float_value = { id, name, desc, RVS_FLOAT_TYPE, value, start, end } \
    }
#define RVSBoolValue(id, name, desc, value)                    \
    {                                                          \
        .bool_value = { id, name, desc, RVS_BOOL_TYPE, value } \
    }
#define RVSIntValue_Range(id, name, desc, value, min, max)                 \
    {                                                                      \
        .int_value = { id, name, desc, RVS_INTEGER_TYPE, value, min, max } \
    }
#define RVSIntValue_DescRange(id, name, desc, value, ranges) \
    {                                                        \
        .int_fixed_value = {                                 \
            id,                                              \
            name,                                            \
            desc,                                            \
            RVS_INTEGER_RANGE_TYPE,                          \
            value,                                           \
            (RVSIntegerRangeValue*)&ranges,                  \
            rv_sizeof_array(ranges)                          \
        }                                                    \
    }
#define RVSStringValue_DescRange(id, name, desc, value, ranges) \
    {                                                           \
        .string_fixed_value = {                                 \
            id,                                                 \
            name,                                               \
            desc,                                               \
            RVS_STRING_RANGE_TYPE,                              \
            value,                                              \
            (RVSStringRangeValue*)&ranges,                      \
            rv_sizeof_array(ranges)                             \
        }                                                       \
    }

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef enum RVSettingsError {
    RVSettingsError_Ok,
    RVSettingsError_NotFound,
    RVSettingsError_DuplicatedId,
    RVSettingsError_WrongType,
} RVSettingsError;

struct RVSPrivate;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVSettingsAPI {
    void* priv_data;

    // Register the settings to be used for the playback plugin
    RVSettingsError (*reg)(void* data, const char* name, const RVSSetting* settings, int count);

    // access settings
    RVSettingsError (*get_string)(void* data, const char* ext, const char* id, const char** value);
    RVSettingsError (*get_int)(void* data, const char* ext, const char* id, int* value);
    RVSettingsError (*get_float)(void* data, const char* ext, const char* id, float* value);
    RVSettingsError (*get_bool)(void* data, const char* ext, const char* id, bool* value);

    // Update settings
    // RVSettingError (*set_string)(void* priv_data, int id, char* value);
    // RVSettingError (*set_int)(void* priv_data, int id, int* value);
    // RVSettingError (*set_float)(void* priv_data, int id, int* value);

    // makes it possible to disable / enable a control
    // RVSettingsError (*enable_ctl)(void* priv_data, int id, bool state);

    // get the last error (null if no error)
    // const char* (*get_last_error)(void* priv_data);

} RVRegisterSettingsAPI;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RVSettings_register(api, name, settings) \
    api->reg(api->priv_data, name, (RVSSetting*)&settings, rv_sizeof_array(settings))

#define RVSettings_get_string(api, ext, id, value) api->get_string(api->priv_data, ext, id, value)
#define RVSettings_get_int(api, ext, id, value) api->get_int(api->priv_data, ext, id, value)
#define RVSettings_get_float(api, ext, id, value) api->get_float(api->priv_data, ext, id, value)
#define RVSettings_get_bool(api, ext, id, value) api->get_bool(api->priv_data, ext, id, value)

// #define RVSettings_get_last_error(api) api->get_last_error(api->priv_data)

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
}
#endif
