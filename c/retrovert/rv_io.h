#pragma once

#include <stdint.h>

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
extern "C" {
#endif

typedef int RVIoErrorCode;
typedef void* RVIoHandle;

typedef enum RVFileSeek {
    RVFileSeek_Start,
    RVFileSeek_Current,
    RVFileSeek_End,
} RVFileSeek;

typedef struct RVIoResult {
    const char* error_message;
    int status;
} RVIoResult;

struct RVIoAPIPrivate;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

typedef struct RVIoAPI {
    // private internal data
    struct RVIoAPIPrivate* api_data;

    // Check if a filename exists.
    int (*exists)(struct RVIoAPIPrivate* api, const char* filename);

    // Load the whole file to memory. dest points to the data and size to the total read size.
    RVIoErrorCode (*read_file_to_memory)(struct RVIoAPIPrivate* api, const char* filename, void** dest, uint64_t* size);
    RVIoErrorCode (*free_file_to_memory)(struct RVIoAPIPrivate* api, void* dest);

    // Io functions for more control
    RVIoErrorCode (*open)(struct RVIoAPIPrivate* api, const char* target, RVIoHandle* handle);
    RVIoErrorCode (*close)(struct RVIoAPIPrivate* api, RVIoHandle handle);
    RVIoErrorCode (*size)(struct RVIoAPIPrivate* api, RVIoHandle handle, uint64_t* res);
    RVIoErrorCode (*read)(struct RVIoAPIPrivate* api, RVIoHandle handle, void* dest, uint64_t size);
    RVIoErrorCode (*seek)(struct RVIoAPIPrivate* api, RVIoHandle handle, RVFileSeek type, int64_t step);
} RVIoAPI;

#define RVIo_read_file_to_memory(api, filename, dest, size) \
    api->read_file_to_memory(api->api_data, filename, dest, size)

#define RVIo_free_file_to_memory(api, dest) api->free_file_to_memory(api->api_data, dest)

#define RV_FILE_API_VERSION 1

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
}
#endif
