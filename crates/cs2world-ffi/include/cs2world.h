#ifndef CS2WORLD_H
#define CS2WORLD_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
#define CS2W_ABI_VERSION 1
typedef uint64_t cs2w_handle;
typedef struct cs2w_stats {
    uint32_t size, version;
    uint64_t up_total, down_total, up_bps, down_bps, active_tcp, active_udp;
    uint64_t wire_up, wire_down;
} cs2w_stats;
/* Callback JSON bytes are UTF-8 and borrowed only for the call.
   Event objects carry command, ok, and detail; command 0 denotes a service event. */
typedef void (*cs2w_callback)(void *context, const uint8_t *bytes, size_t len);
uint32_t cs2w_abi_version(void);
cs2w_handle cs2w_create(void);
int32_t cs2w_destroy(cs2w_handle);
/* Commands copy inputs and return a positive command ID or a negative error.
   -1 = invalid handle, -2 = invalid input, -3 = busy, -4 = closed/internal error. */
int64_t cs2w_start(cs2w_handle, const uint8_t *json, size_t len);
int64_t cs2w_reload(cs2w_handle, const uint8_t *json, size_t len);
int64_t cs2w_stop(cs2w_handle);
int64_t cs2w_preconnect(cs2w_handle);
int64_t cs2w_probe_a2s(cs2w_handle, const uint8_t *host, size_t len, uint16_t port);
int64_t cs2w_start_desktop_tun(cs2w_handle);
/* Initialize size to sizeof(cs2w_stats) and version to CS2W_ABI_VERSION. */
int32_t cs2w_get_stats(cs2w_handle, cs2w_stats *out);
/* Returning from registration/unregistration guarantees the previous callback has drained.
   Unregister with NULL. Call these and destroy off the dispatch callback thread. */
int32_t cs2w_set_event_callback(cs2w_handle, cs2w_callback, void *context);
int32_t cs2w_set_log_callback(cs2w_handle, cs2w_callback, void *context);
#ifdef __cplusplus
}
#endif
#endif
