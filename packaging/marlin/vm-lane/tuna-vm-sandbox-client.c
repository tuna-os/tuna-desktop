/* CI-only Wayland client for V-INTROSPECT's sandbox identity (#410).
 *
 *   tuna-vm-sandbox-client APP_ID
 *
 * Maps one 320x200 xdg toplevel with the given app_id and keeps it mapped
 * until killed. The same binary runs inside a Flatpak sandbox (as the
 * org.tuna.VmSandbox app from the lane's local repository) and unsandboxed
 * claiming that same app_id, so only the compositor's authenticated process
 * metadata can tell the two apart. Original code. */
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client.h"

#define WIDTH 320
#define HEIGHT 200

static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm_base;
static int configured;

static void ping(void *data, struct xdg_wm_base *base, uint32_t serial) {
    (void)data;
    xdg_wm_base_pong(base, serial);
}
static const struct xdg_wm_base_listener wm_base_listener = {.ping = ping};

static void global(void *data, struct wl_registry *registry, uint32_t name,
                   const char *interface, uint32_t version) {
    (void)data;
    (void)version;
    if (strcmp(interface, wl_compositor_interface.name) == 0)
        compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
    else if (strcmp(interface, wl_shm_interface.name) == 0)
        shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    else if (strcmp(interface, xdg_wm_base_interface.name) == 0) {
        wm_base = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(wm_base, &wm_base_listener, NULL);
    }
}
static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data;
    (void)registry;
    (void)name;
}
static const struct wl_registry_listener registry_listener = {global, global_remove};

static void surface_configure(void *data, struct xdg_surface *surface, uint32_t serial) {
    (void)data;
    xdg_surface_ack_configure(surface, serial);
    configured = 1;
}
static const struct xdg_surface_listener surface_listener = {surface_configure};

static void toplevel_configure(void *data, struct xdg_toplevel *toplevel, int32_t w, int32_t h,
                               struct wl_array *states) {
    (void)data;
    (void)toplevel;
    (void)w;
    (void)h;
    (void)states;
}
static void toplevel_close(void *data, struct xdg_toplevel *toplevel) {
    (void)data;
    (void)toplevel;
    exit(0);
}
// Designated: newer xdg-shell headers add configure_bounds/wm_capabilities,
// events the compositor never sends at the version 1 this client binds.
static const struct xdg_toplevel_listener toplevel_listener = {.configure = toplevel_configure,
                                                               .close = toplevel_close};

static struct wl_buffer *buffer(void) {
    int stride = WIDTH * 4, size = stride * HEIGHT;
    int fd = memfd_create("tuna-vm-sandbox", MFD_CLOEXEC);
    if (fd < 0 || ftruncate(fd, size) < 0) {
        perror("memfd");
        exit(1);
    }
    uint32_t *pixels = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) {
        perror("mmap");
        exit(1);
    }
    for (int i = 0; i < WIDTH * HEIGHT; i++)
        pixels[i] = 0xff3a7f5c;
    munmap(pixels, size);
    struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, size);
    struct wl_buffer *result =
        wl_shm_pool_create_buffer(pool, 0, WIDTH, HEIGHT, stride, WL_SHM_FORMAT_XRGB8888);
    wl_shm_pool_destroy(pool);
    close(fd);
    return result;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: tuna-vm-sandbox-client APP_ID\n");
        return 2;
    }
    struct wl_display *display = wl_display_connect(NULL);
    if (!display) {
        fprintf(stderr, "no Wayland display: %s\n", strerror(errno));
        return 1;
    }
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    wl_display_roundtrip(display);
    if (!compositor || !shm || !wm_base) {
        fprintf(stderr, "missing wl_compositor, wl_shm or xdg_wm_base\n");
        return 1;
    }
    struct wl_surface *surface = wl_compositor_create_surface(compositor);
    struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(wm_base, surface);
    xdg_surface_add_listener(xdg, &surface_listener, NULL);
    struct xdg_toplevel *toplevel = xdg_surface_get_toplevel(xdg);
    xdg_toplevel_add_listener(toplevel, &toplevel_listener, NULL);
    xdg_toplevel_set_app_id(toplevel, argv[1]);
    xdg_toplevel_set_title(toplevel, "Tuna VM sandbox client");
    wl_surface_commit(surface);
    while (!configured)
        if (wl_display_dispatch(display) < 0)
            return 1;
    wl_surface_attach(surface, buffer(), 0, 0);
    wl_surface_damage(surface, 0, 0, WIDTH, HEIGHT);
    wl_surface_commit(surface);
    while (wl_display_dispatch(display) >= 0) {
    }
    return 0;
}
