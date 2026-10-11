/* Actual xdg_system_bell_v1 request. Generated bindings are built in artifacts. */
#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <wayland-client.h>
#include "xdg-bell-client.h"
static struct xdg_system_bell_v1 *bell;
static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data; (void)version;
    if (strcmp(interface, "xdg_system_bell_v1") == 0)
        bell = wl_registry_bind(registry, name, &xdg_system_bell_v1_interface, 1);
}
static void removed(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
int main(void) {
    struct wl_display *display = wl_display_connect(NULL);
    assert(display);
    struct wl_registry *registry = wl_display_get_registry(display);
    static const struct wl_registry_listener listener = {global, removed};
    wl_registry_add_listener(registry, &listener, NULL);
    assert(wl_display_roundtrip(display) >= 0 && bell);
    xdg_system_bell_v1_ring(bell, NULL);
    assert(wl_display_roundtrip(display) >= 0);
    puts("xdg_system_bell_v1 ring request acknowledged by display roundtrip");
    xdg_system_bell_v1_destroy(bell);
    wl_registry_destroy(registry);
    wl_display_disconnect(display);
    return 0;
}
