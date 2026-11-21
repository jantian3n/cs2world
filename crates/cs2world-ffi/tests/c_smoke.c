#include "cs2world.h"
#include <stdio.h>
#include <string.h>

struct observed { int events; int errors; };

static void on_event(void *ctx, const uint8_t *data, size_t len) {
    struct observed *obs = ctx;
    char message[1024];
    if (len >= sizeof(message)) { obs->errors++; return; }
    memcpy(message, data, len);
    message[len] = 0;
    if (!strstr(message, "\"ok\":true")) obs->errors++;
    obs->events++;
}

int main(void) {
    const char config[] = "{\"inbounds\":[{\"type\":\"mixed\",\"listen_port\":0}],\"outbounds\":[{\"type\":\"direct\"}]}";
    cs2w_handle handle = cs2w_create();
    struct observed obs = {0};
    cs2w_stats stats = {0};
    stats.size = (uint32_t)sizeof(stats);
    stats.version = CS2W_ABI_VERSION;
    if (!handle || cs2w_abi_version() != CS2W_ABI_VERSION) return 1;
    if (cs2w_set_event_callback(handle, on_event, &obs) != 0) return 2;
    if (cs2w_start(handle, (const uint8_t *)config, strlen(config)) <= 0) return 3;
    if (cs2w_get_stats(handle, &stats) != 0 || cs2w_stop(handle) <= 0) return 4;
    if (cs2w_destroy(handle) != 0 || obs.errors || obs.events < 2) return 5;
    puts("C ABI passed: start, stats, stop, destroy, callback drain");
    return 0;
}
