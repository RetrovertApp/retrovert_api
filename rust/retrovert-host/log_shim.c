#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#define RV_LOG_FORMAT_LIMIT 2047

// Formatted message handed back to the Rust side of the log service.
extern void retrovert_host_log_write(void* self, uint32_t level, const char* file, int line, const char* text);

// RVLog::log is printf-style variadic, which stable Rust can call but cannot define.
// This shim does the formatting and hands the finished line back.
void retrovert_host_log_shim(void* self, uint32_t level, const char* file, int line, const char* fmt, ...) {
    if (fmt == NULL) {
        return;
    }

    size_t format_length = 0;
    while (format_length <= RV_LOG_FORMAT_LIMIT && fmt[format_length] != '\0') {
        ++format_length;
    }
    if (format_length > RV_LOG_FORMAT_LIMIT) {
        return;
    }

    char buf[2048];
    va_list args;
    va_start(args, fmt);
    vsnprintf(buf, sizeof(buf), fmt, args);
    va_end(args);

    retrovert_host_log_write(self, level, file, line, buf);
}
