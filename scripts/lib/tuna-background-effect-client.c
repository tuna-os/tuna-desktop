/* Real shm checkerboard + transparent synchronized subsurface blur consumer.
 * Commands on stdin; JSON receipts on stdout. A receipt is not pixel proof.
 * The proof driver captures the compositor output after the frame callback.
 */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"
#include "ext-background-effect-v1-client-protocol.h"

/* Early staging XML accidentally defined Blur=0; such a header cannot
 * distinguish support from an empty capability set. Never fake admission. */
_Static_assert(EXT_BACKGROUND_EFFECT_MANAGER_V1_CAPABILITY_BLUR == 1,
               "update wayland-protocols: background-effect blur capability must be bit 1");

#define WIDTH 384
#define HEIGHT 256
struct client {
    struct wl_display *display;
    struct wl_compositor *compositor;
    struct wl_subcompositor *subcompositor;
    struct wl_shm *shm;
    struct xdg_wm_base *wm;
    struct ext_background_effect_manager_v1 *effects;
    unsigned capabilities, configured, frame_done;
};
static void ping(void *data, struct xdg_wm_base *wm, uint32_t serial) {
    (void)data; xdg_wm_base_pong(wm, serial);
}
static const struct xdg_wm_base_listener wm_listener = { .ping = ping };
static void capabilities(void *data, struct ext_background_effect_manager_v1 *manager, uint32_t flags) {
    (void)manager; ((struct client *)data)->capabilities = flags;
}
static const struct ext_background_effect_manager_v1_listener effect_listener = { .capabilities = capabilities };
static void global(void *data, struct wl_registry *registry, uint32_t id,
                   const char *name, uint32_t version) {
    struct client *c = data;
    if (!strcmp(name, "wl_compositor"))
        c->compositor = wl_registry_bind(registry, id, &wl_compositor_interface, version < 4 ? version : 4);
    else if (!strcmp(name, "wl_subcompositor"))
        c->subcompositor = wl_registry_bind(registry, id, &wl_subcompositor_interface, 1);
    else if (!strcmp(name, "wl_shm"))
        c->shm = wl_registry_bind(registry, id, &wl_shm_interface, 1);
    else if (!strcmp(name, "xdg_wm_base")) {
        c->wm = wl_registry_bind(registry, id, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(c->wm, &wm_listener, c);
    } else if (!strcmp(name, "ext_background_effect_manager_v1")) {
        c->effects = wl_registry_bind(registry, id, &ext_background_effect_manager_v1_interface, 1);
        ext_background_effect_manager_v1_add_listener(c->effects, &effect_listener, c);
    }
}
static void removed(void *data, struct wl_registry *registry, uint32_t id) {
    (void)data; (void)registry; (void)id;
}
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = removed };
static void configure(void *data, struct xdg_surface *surface, uint32_t serial) {
    xdg_surface_ack_configure(surface, serial); ((struct client *)data)->configured++;
}
static const struct xdg_surface_listener surface_listener = { .configure = configure };
static void top_configure(void *data, struct xdg_toplevel *top, int32_t width,
                          int32_t height, struct wl_array *states) {
    (void)data; (void)top; (void)width; (void)height; (void)states;
}
static void close_top(void *data, struct xdg_toplevel *top) { (void)data; (void)top; }
static const struct xdg_toplevel_listener top_listener = { .configure = top_configure, .close = close_top };
static void frame_done(void *data, struct wl_callback *callback, uint32_t time) {
    (void)time; ((struct client *)data)->frame_done = 1; wl_callback_destroy(callback);
}
static const struct wl_callback_listener frame_listener = { .done = frame_done };
static void die(const char *message) { perror(message); exit(1); }
static struct wl_buffer *buffer(struct client *c, int transparent) {
    const size_t bytes = WIDTH * HEIGHT * 4;
    char path[] = "/tmp/tuna-background-shm-XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) die("mkstemp");
    unlink(path);
    if (ftruncate(fd, (off_t)bytes) < 0) die("ftruncate");
    uint32_t *pixels = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (pixels == MAP_FAILED) die("mmap");
    for (int y = 0; y < HEIGHT; y++) for (int x = 0; x < WIDTH; x++) {
        uint32_t color = ((x / 4 + y / 4) % 2) ? 0xffffffff : 0xff000000;
        if (x < 8 || y < 8 || x >= WIDTH - 8 || y >= HEIGHT - 8) color = 0xffff00ff;
        pixels[y * WIDTH + x] = transparent ? 0 : color;
    }
    struct wl_shm_pool *pool = wl_shm_create_pool(c->shm, fd, (int)bytes);
    struct wl_buffer *result = wl_shm_pool_create_buffer(pool, 0, WIDTH, HEIGHT, WIDTH * 4,
                                                        WL_SHM_FORMAT_ARGB8888);
    wl_shm_pool_destroy(pool); munmap(pixels, bytes); close(fd);
    return result;
}
static void present(struct client *c, struct wl_surface *parent) {
    c->frame_done = 0;
    struct wl_callback *callback = wl_surface_frame(parent);
    wl_callback_add_listener(callback, &frame_listener, c);
    wl_surface_damage(parent, 0, 0, WIDTH, HEIGHT);
    wl_surface_commit(parent);
    while (!c->frame_done) if (wl_display_dispatch(c->display) < 0) die("frame dispatch");
}
static void set_region(struct client *c, struct ext_background_effect_surface_v1 *effect) {
    struct wl_region *region = wl_compositor_create_region(c->compositor);
    /* Clip negative coordinates to the surface, preserve the subtract hole.
     * Expected visible region: [0,288)x[0,224), less [128,160)x[96,128).
     */
    wl_region_add(region, -32, -32, 320, 256);
    wl_region_subtract(region, 128, 96, 32, 32);
    ext_background_effect_surface_v1_set_blur_region(effect, region);
    /* Mutating and destroying the source must not alter the copied region. */
    wl_region_subtract(region, -32, -32, 320, 256);
    wl_region_destroy(region);
}
int main(int argc, char **argv) {
    (void)argv;
    if (argc != 1) { fprintf(stderr, "usage: tuna-background-effect-client < COMMANDS\n"); return 2; }
    setvbuf(stdout, NULL, _IOLBF, 0);
    struct client c = { .display = wl_display_connect(NULL) };
    if (!c.display) die("wl_display_connect");
    struct wl_registry *registry = wl_display_get_registry(c.display);
    wl_registry_add_listener(registry, &registry_listener, &c);
    if (wl_display_roundtrip(c.display) < 0 || wl_display_roundtrip(c.display) < 0) die("registry");
    if (!c.effects || !(c.capabilities & EXT_BACKGROUND_EFFECT_MANAGER_V1_CAPABILITY_BLUR)) {
        puts("{\"result\":\"unsupported\",\"reason\":\"blur-capability-not-admitted\"}");
        wl_display_disconnect(c.display); return 77;
    }
    if (!c.compositor || !c.subcompositor || !c.shm || !c.wm) {
        fprintf(stderr, "required rendering globals unavailable\n"); return 1;
    }
    struct wl_surface *parent = wl_compositor_create_surface(c.compositor);
    struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(c.wm, parent);
    xdg_surface_add_listener(xdg, &surface_listener, &c);
    struct xdg_toplevel *top = xdg_surface_get_toplevel(xdg);
    xdg_toplevel_add_listener(top, &top_listener, &c);
    xdg_toplevel_set_title(top, "Tuna background-effect pixel consumer");
    xdg_toplevel_set_app_id(top, "org.tuna.BackgroundEffectProof");
    xdg_toplevel_set_min_size(top, WIDTH, HEIGHT); xdg_toplevel_set_max_size(top, WIDTH, HEIGHT);
    xdg_surface_set_window_geometry(xdg, 0, 0, WIDTH, HEIGHT);
    wl_surface_commit(parent);
    while (!c.configured) if (wl_display_dispatch(c.display) < 0) die("configure");
    struct wl_surface *child = wl_compositor_create_surface(c.compositor);
    struct wl_subsurface *sub = wl_subcompositor_get_subsurface(c.subcompositor, child, parent);
    wl_subsurface_set_position(sub, 0, 0); /* Synchronized by default. */
    struct wl_buffer *background = buffer(&c, 0), *transparent = buffer(&c, 1);
    wl_surface_attach(parent, background, 0, 0);
    wl_surface_attach(child, transparent, 0, 0);
    wl_surface_damage(child, 0, 0, WIDTH, HEIGHT); wl_surface_commit(child);
    struct ext_background_effect_surface_v1 *effect =
        ext_background_effect_manager_v1_get_background_effect(c.effects, child);
    present(&c, parent);
    puts("{\"result\":\"ready\",\"width\":384,\"height\":256,\"buffer\":\"wl_shm-argb8888\",\"synchronized_subsurface\":true}");
    char command[64]; unsigned sequence = 0;
    while (fgets(command, sizeof command, stdin)) {
        command[strcspn(command, "\r\n")] = 0;
        if (!strcmp(command, "quit")) break;
        if (!strcmp(command, "on-pending")) {
            if (!effect) effect = ext_background_effect_manager_v1_get_background_effect(c.effects, child);
            set_region(&c, effect);
        } else if (!strcmp(command, "null-pending") && effect) {
            ext_background_effect_surface_v1_set_blur_region(effect, NULL);
        } else if (!strcmp(command, "destroy-pending") && effect) {
            ext_background_effect_surface_v1_destroy(effect); effect = NULL;
        } else if (!strcmp(command, "child-commit")) {
            wl_surface_commit(child);
        } else if (!strcmp(command, "unmap") || !strcmp(command, "remap")) {
            wl_surface_attach(child, !strcmp(command, "unmap") ? NULL : transparent, 0, 0);
            wl_surface_damage(child, 0, 0, WIDTH, HEIGHT);
            wl_surface_commit(child);
        } else if (!strcmp(command, "parent-commit")) {
            /* Cached synchronized child state only becomes visible here. */
        } else if (!strcmp(command, "commit")) {
            wl_surface_commit(child);
        } else { fprintf(stderr, "invalid command or object state: %s\n", command); return 2; }
        if (!strcmp(command, "commit") || !strcmp(command, "parent-commit") ||
            !strcmp(command, "unmap") || !strcmp(command, "remap")) present(&c, parent);
        if (wl_display_roundtrip(c.display) < 0) die("command roundtrip");
        if (!(c.capabilities & EXT_BACKGROUND_EFFECT_MANAGER_V1_CAPABILITY_BLUR)) {
            puts("{\"result\":\"unsupported\",\"reason\":\"blur-capability-revoked\"}"); return 77;
        }
        printf("{\"result\":\"command-applied\",\"command\":\"%s\",\"sequence\":%u}\n", command, ++sequence);
    }
    if (effect) ext_background_effect_surface_v1_destroy(effect);
    wl_subsurface_destroy(sub); wl_surface_destroy(child);
    xdg_toplevel_destroy(top); xdg_surface_destroy(xdg); wl_surface_destroy(parent);
    wl_buffer_destroy(transparent); wl_buffer_destroy(background);
    wl_display_roundtrip(c.display); wl_display_disconnect(c.display);
    return 0;
}
