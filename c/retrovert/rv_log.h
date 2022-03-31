#pragma once

#ifdef _cplusplus
extern "C" {
#endif

enum { RV_LOG_TRACE, RV_LOG_DEBUG, RV_LOG_INFO, RV_LOG_WARN, RV_LOG_ERROR, RV_LOG_FATAL };

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define RV_LOG_API_VERSION 1

struct RVLogAPIPrivate;

typedef struct RVLogAPI {
    RVLogAPIPrivate* priv_data;
    // Set basename appended to output (useful for setting plugin name)
    void (*log_set_base_name)(struct RVLogAPIPrivate* priv_data, const char* base_name);
    // Write to the log. It's recommended to use the macros bellow for eaiser usage
    void (*log)(struct RVLogAPIPrivate* priv_data, int level, const char* file, int line, const char* fmt, ...);
} RVLogAPI;

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#define rv_debug(...)                                                           \
    {                                                                           \
        extern RVLogAPI* g_rv_log;                                              \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_DEBUG, NULL, 0, __VA_ARGS__); \
    }

#define rvfl_debug(...)                                                                    \
    {                                                                                      \
        extern RVLogAPI* g_rv_log;                                                         \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_DEBUG, __FILE__, __LINE__, __VA_ARGS__); \
    }

#define rv_trace(...)                                                           \
    {                                                                           \
        extern RVLogAPI* g_rv_log;                                              \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_TRACE, NULL, 0, __VA_ARGS__); \
    }

#define rvfl_trace(...)                                                                    \
    {                                                                                      \
        extern RVLogAPI* g_rv_log;                                                         \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_TRACE, __FILE__, __LINE__, __VA_ARGS__); \
    }

#define rv_info(...)                                                           \
    {                                                                          \
        extern RVLogAPI* g_rv_log;                                             \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_INFO, NULL, 0, __VA_ARGS__); \
    }

#define rvfl_info(...)                                                                    \
    {                                                                                     \
        extern RVLogAPI* g_rv_log;                                                        \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_INFO, __FILE__, __LINE__, __VA_ARGS__); \
    }

#define rv_warn(...)                                                           \
    {                                                                          \
        extern RVLogAPI* g_rv_log;                                             \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_WARN, NULL, 0, __VA_ARGS__); \
    }

#define rvfl_warn(...)                                                                    \
    {                                                                                     \
        extern RVLogAPI* g_rv_log;                                                        \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_WARN, __FILE__, __LINE__, __VA_ARGS__); \
    }

#define rv_error(...)                                                           \
    {                                                                           \
        extern RVLogAPI* g_rv_log;                                              \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_ERROR, NULL, 0, __VA_ARGS__); \
    }

#define rvfl_error(...)                                                                    \
    {                                                                                      \
        extern RVLogAPI* g_rv_log;                                                         \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_ERROR, __FILE__, __LINE__, __VA_ARGS__); \
    }

#define rv_fatal(...)                                                           \
    {                                                                           \
        extern RVLogAPI* g_rv_log;                                              \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_FATAL, NULL, 0, __VA_ARGS__); \
    }

#define rvfl_fatal(...)                                                                    \
    {                                                                                      \
        extern RVLogAPI* g_rv_log;                                                         \
        g_rv_log->log(g_rv_log->priv_data, RV_LOG_FATAL, __FILE__, __LINE__, __VA_ARGS__); \
    }

///////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

#ifdef _cplusplus
}
#endif
