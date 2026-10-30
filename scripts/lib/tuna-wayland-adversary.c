/* Bounded live protocol failures and floods. Server survival is checked by the caller. */
#define _GNU_SOURCE
#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>
#include <wayland-client.h>
#include "fuzz-interfaces.h"
#include "xdg-shell-client-protocol.h"

struct client {
    struct wl_display *display;
    struct wl_compositor *compositor;
    struct wl_shm *shm;
    struct xdg_wm_base *wm;
    unsigned configured;
    uint32_t serial;
    unsigned released;
    unsigned frames;
};
static void ping(void *data, struct xdg_wm_base *wm, uint32_t serial) {
    (void)data; xdg_wm_base_pong(wm, serial);
}
static const struct xdg_wm_base_listener wm_listener = { .ping = ping };
static void global(void *data, struct wl_registry *registry, uint32_t id,
                   const char *name, uint32_t version) {
    struct client *c = data;
    if (!strcmp(name, "wl_compositor"))
        c->compositor = wl_registry_bind(registry, id, &wl_compositor_interface, version < 4 ? version : 4);
    else if (!strcmp(name, "wl_shm"))
        c->shm = wl_registry_bind(registry, id, &wl_shm_interface, 1);
    else if (!strcmp(name, "xdg_wm_base")) {
        c->wm = wl_registry_bind(registry, id, &xdg_wm_base_interface, 1);
        xdg_wm_base_add_listener(c->wm, &wm_listener, c);
    }
}
static void removed(void *data, struct wl_registry *registry, uint32_t id) {
    (void)data; (void)registry; (void)id;
}
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = removed };
static void configure(void *data, struct xdg_surface *surface, uint32_t serial) {
    (void)surface; ((struct client *)data)->configured++;
    ((struct client *)data)->serial = serial;
    /* Deliberately do not acknowledge: each mode decides its next request. */
}
static const struct xdg_surface_listener surface_listener = { .configure = configure };
static void top_configure(void *data, struct xdg_toplevel *top, int32_t width,
                          int32_t height, struct wl_array *states) {
    (void)data; (void)top; (void)width; (void)height; (void)states;
}
static void close_top(void *data, struct xdg_toplevel *top) { (void)data; (void)top; }
static const struct xdg_toplevel_listener top_listener = { .configure = top_configure, .close = close_top };
static int require_error(struct client *c, const char *mode, const char *name, uint32_t code) {
    int roundtrip = wl_display_roundtrip(c->display);
    int error = wl_display_get_error(c->display);
    if (roundtrip >= 0 || error != EPROTO) {
        fprintf(stderr, "%s: expected protocol rejection, roundtrip=%d error=%d (%s)\n", mode, roundtrip, error, strerror(error)); return 1;
    }
    const struct wl_interface *interface = NULL;
    uint32_t object = 0;
    uint32_t actual = wl_display_get_protocol_error(c->display, &interface, &object);
    if (!interface || strcmp(interface->name, name) || actual != code || !object) {
        fprintf(stderr, "%s: wrong rejection %s code %u object %u\n", mode,
                interface ? interface->name : "none", actual, object); return 1;
    }
    printf("{\"mode\":\"%s\",\"result\":\"protocol-rejected\",\"interface\":\"%s\",\"code\":%u}\n", mode, name, code);
    return 0;
}
static long long now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (long long)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}
/* Flush everything queued and dispatch whatever arrived, waiting at most
 * timeout_ms for the socket. 1 = fully flushed, 0 = still pending, -1 = lost. */
static int service(struct wl_display *d, int timeout_ms) {
    while (wl_display_prepare_read(d) != 0)
        if (wl_display_dispatch_pending(d) < 0) return -1;
    int flushed = wl_display_flush(d);
    if (flushed < 0 && errno != EAGAIN) { wl_display_cancel_read(d); return -1; }
    struct pollfd p = { .fd = wl_display_get_fd(d), .events = POLLIN | (flushed < 0 ? POLLOUT : 0) };
    if (poll(&p, 1, timeout_ms) < 0) { wl_display_cancel_read(d); return -1; }
    if (p.revents & POLLIN) {
        if (wl_display_read_events(d) < 0) return -1;
    } else wl_display_cancel_read(d);
    if (wl_display_dispatch_pending(d) < 0) return -1;
    return flushed >= 0;
}
/* Never let libwayland's bounded output buffer fill: a server that stops
 * reading for five seconds is a wedged compositor, reported as an error. */
static int drain(struct wl_display *d) {
    long long deadline = now_ms() + 5000;
    for (;;) {
        int r = service(d, 50);
        if (r != 0) return r < 0 ? -1 : 0;
        if (now_ms() > deadline) { errno = ETIMEDOUT; return -1; }
    }
}
static void sync_done(void *data, struct wl_callback *cb, uint32_t time) {
    (void)time; *(int *)data = 1; wl_callback_destroy(cb);
}
static const struct wl_callback_listener sync_listener = { .done = sync_done };
/* A roundtrip that fails instead of hanging when the server never answers. */
static int bounded_roundtrip(struct wl_display *d, int timeout_ms) {
    int done = 0;
    wl_callback_add_listener(wl_display_sync(d), &sync_listener, &done);
    long long deadline = now_ms() + timeout_ms;
    while (!done) {
        long long left = deadline - now_ms();
        if (left <= 0) { errno = ETIMEDOUT; return -1; }
        if (service(d, (int)(left < 50 ? left : 50)) < 0) return -1;
    }
    return 0;
}
static void frame_done(void *data, struct wl_callback *cb, uint32_t time) {
    (void)time; ((struct client *)data)->frames++; wl_callback_destroy(cb);
}
static const struct wl_callback_listener frame_listener = { .done = frame_done };
static void buffer_release(void *data, struct wl_buffer *buffer) {
    (void)buffer; ((struct client *)data)->released++;
}
static const struct wl_buffer_listener buffer_listener = { .release = buffer_release };
/* A memfd whose pages are actually written, so churn costs real memory. */
static int filled_memfd(size_t size, unsigned char value) {
    int fd = memfd_create("tuna-adversary", MFD_CLOEXEC);
    if (fd < 0) return -1;
    if (ftruncate(fd, (off_t)size) < 0) { close(fd); return -1; }
    void *map = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) { close(fd); return -1; }
    memset(map, value, size);
    munmap(map, size);
    return fd;
}
static struct wl_buffer *shm_buffer(struct client *c, int width, int height, unsigned char value) {
    size_t size = (size_t)width * height * 4;
    int fd = filled_memfd(size, value);
    if (fd < 0) return NULL;
    struct wl_shm_pool *pool = wl_shm_create_pool(c->shm, fd, (int32_t)size);
    close(fd);
    struct wl_buffer *buffer = wl_shm_pool_create_buffer(pool, 0, width, height, width * 4,
                                                         WL_SHM_FORMAT_XRGB8888);
    /* The buffer keeps the pool's memory alive; churn drops the pool at once. */
    wl_shm_pool_destroy(pool);
    wl_buffer_add_listener(buffer, &buffer_listener, c);
    return buffer;
}
/* Map an ordinary toplevel with its first buffer. */
static int map_toplevel(struct client *c, struct wl_surface *surface, struct xdg_surface *xdg,
                        struct wl_buffer *buffer) {
    wl_surface_commit(surface);
    for (unsigned i = 0; i < 4 && !c->configured; i++)
        if (bounded_roundtrip(c->display, 5000) < 0) return -1;
    if (!c->configured) return -1;
    xdg_surface_ack_configure(xdg, c->serial);
    wl_surface_attach(surface, buffer, 0, 0);
    wl_surface_commit(surface);
    return bounded_roundtrip(c->display, 5000);
}
/* Connections opened together, held, then dropped together. */
static int connect_storm(const char *mode) {
    enum { WAVES = 4, PEERS = 250 };
    unsigned served = 0;
    for (unsigned wave = 0; wave < WAVES; wave++) {
        struct wl_display *peers[PEERS];
        int done[PEERS] = { 0 };
        for (unsigned i = 0; i < PEERS; i++) {
            peers[i] = wl_display_connect(NULL);
            if (!peers[i]) {
                fprintf(stderr, "%s: connection %u of wave %u refused: %s\n", mode, i, wave, strerror(errno));
                return 1;
            }
            (void)wl_display_get_registry(peers[i]);
            wl_callback_add_listener(wl_display_sync(peers[i]), &sync_listener, &done[i]);
            if (wl_display_flush(peers[i]) < 0) { perror("flush storm peer"); return 1; }
        }
        long long deadline = now_ms() + 10000;
        for (unsigned i = 0; i < PEERS; i++) {
            while (!done[i]) {
                if (now_ms() > deadline || service(peers[i], 50) < 0) {
                    fprintf(stderr, "%s: connection %u of wave %u was never served\n", mode, i, wave);
                    return 1;
                }
            }
            served++;
        }
        for (unsigned i = 0; i < PEERS; i++) wl_display_disconnect(peers[i]);
    }
    printf("{\"mode\":\"%s\",\"result\":\"all-connections-served\",\"connections\":%u,\"concurrent\":%u}\n",
           mode, served, (unsigned)PEERS);
    return 0;
}
/* Commit as fast as the socket allows, ignoring frame pacing. */
static int commit_flood(struct client *c, const char *mode, struct wl_surface *surface,
                        struct xdg_surface *xdg) {
    enum { COMMITS = 5000 };
    struct wl_buffer *buffers[2] = { shm_buffer(c, 256, 256, 0x40), shm_buffer(c, 256, 256, 0xc0) };
    if (!buffers[0] || !buffers[1] || map_toplevel(c, surface, xdg, buffers[0]) < 0) {
        fprintf(stderr, "%s: could not map the flooding toplevel\n", mode); return 1;
    }
    for (unsigned i = 0; i < COMMITS; i++) {
        wl_surface_attach(surface, buffers[i & 1], 0, 0);
        wl_surface_damage_buffer(surface, 0, 0, 256, 256);
        wl_callback_add_listener(wl_surface_frame(surface), &frame_listener, c);
        wl_surface_commit(surface);
        if (drain(c->display) < 0) { fprintf(stderr, "%s: lost at commit %u: %s\n", mode, i, strerror(errno)); return 1; }
    }
    /* The surface must keep receiving frames once the flood stops. */
    unsigned before = c->frames;
    wl_callback_add_listener(wl_surface_frame(surface), &frame_listener, c);
    wl_surface_damage_buffer(surface, 0, 0, 256, 256);
    wl_surface_commit(surface);
    long long deadline = now_ms() + 5000;
    while (c->frames == before) {
        if (now_ms() > deadline || service(c->display, 50) < 0) {
            fprintf(stderr, "%s: no frame callback after the flood\n", mode); return 1;
        }
    }
    printf("{\"mode\":\"%s\",\"result\":\"flood-served\",\"commits\":%u,\"frames\":%u,\"releases\":%u}\n",
           mode, (unsigned)COMMITS, c->frames, c->released);
    return 0;
}
/* Fresh, fully written 2 MiB buffers dropped right after their commit. */
static int buffer_churn(struct client *c, const char *mode, struct wl_surface *surface,
                        struct xdg_surface *xdg) {
    enum { BUFFERS = 300, WIDTH = 1024, HEIGHT = 512 };
    struct wl_buffer *first = shm_buffer(c, WIDTH, HEIGHT, 0);
    if (!first || map_toplevel(c, surface, xdg, first) < 0) {
        fprintf(stderr, "%s: could not map the churning toplevel\n", mode); return 1;
    }
    wl_buffer_destroy(first);
    for (unsigned i = 0; i < BUFFERS; i++) {
        struct wl_buffer *buffer = shm_buffer(c, WIDTH, HEIGHT, (unsigned char)i);
        if (!buffer) { perror("allocate churn buffer"); return 1; }
        wl_surface_attach(surface, buffer, 0, 0);
        wl_surface_damage_buffer(surface, 0, 0, WIDTH, HEIGHT);
        wl_surface_commit(surface);
        wl_buffer_destroy(buffer);
        if (drain(c->display) < 0 || (i % 25 == 24 && bounded_roundtrip(c->display, 5000) < 0)) {
            fprintf(stderr, "%s: lost at buffer %u\n", mode, i); return 1;
        }
    }
    if (bounded_roundtrip(c->display, 5000) < 0) { fprintf(stderr, "%s: lost after churn\n", mode); return 1; }
    printf("{\"mode\":\"%s\",\"result\":\"churn-served\",\"buffers\":%u,\"bytes\":%llu}\n",
           mode, (unsigned)BUFFERS, (unsigned long long)BUFFERS * WIDTH * HEIGHT * 4);
    return 0;
}
/* Fuzz: seeded random requests on every bound global and the objects
 * they create, with arguments shaped by each request's signature (edge
 * integers, wrong-typed and stale objects, null, arrays, fds). Protocol
 * errors and disconnects are expected outcomes; a session the server stops
 * answering is a failure. */
enum { FUZZ_LIVE = 256, FUZZ_ARGS = 20, FUZZ_SESSIONS = 100, FUZZ_STEPS = 48 };
struct fuzz {
    struct wl_display *display;
    uint64_t rng;
    struct wl_proxy *live[FUZZ_LIVE];
    const struct wl_interface *iface[FUZZ_LIVE];
    unsigned count;
};
static uint64_t fuzz_next(struct fuzz *f) {
    f->rng ^= f->rng << 13; f->rng ^= f->rng >> 7; f->rng ^= f->rng << 17;
    return f->rng;
}
static uint32_t fuzz_pick(struct fuzz *f, uint32_t n) { return n ? (uint32_t)(fuzz_next(f) % n) : 0; }
/* Mostly small values (enums, counts), then edges, then anything. */
static int32_t fuzz_int(struct fuzz *f) {
    static const int32_t edges[] = { -1, 7, 16, 64, 256, 4096, 65535, INT32_MAX, INT32_MIN };
    uint32_t kind = fuzz_pick(f, 10);
    if (kind < 5) return (int32_t)fuzz_pick(f, 5);
    if (kind < 8) return edges[fuzz_pick(f, sizeof edges / sizeof *edges)];
    return (int32_t)fuzz_next(f);
}
static void fuzz_global(void *data, struct wl_registry *registry, uint32_t id,
                        const char *name, uint32_t version) {
    struct fuzz *f = data;
    for (const struct wl_interface *const *i = fuzz_interfaces; *i && f->count < FUZZ_LIVE; i++) {
        if (strcmp((*i)->name, name)) continue;
        uint32_t bound = version < (uint32_t)(*i)->version ? version : (uint32_t)(*i)->version;
        f->live[f->count] = wl_registry_bind(registry, id, *i, bound);
        f->iface[f->count++] = *i;
        return;
    }
}
static const struct wl_registry_listener fuzz_registry = { .global = fuzz_global, .global_remove = removed };
/* One random request on a random live object; 1 when one was sent. */
static int fuzz_request(struct fuzz *f) {
    unsigned index = fuzz_pick(f, f->count);
    struct wl_proxy *proxy = f->live[index];
    const struct wl_interface *iface = f->iface[index];
    if (!iface->method_count) return 0;
    uint32_t opcode = fuzz_pick(f, (uint32_t)iface->method_count);
    const struct wl_message *message = &iface->methods[opcode];
    const char *sig = message->signature;
    uint32_t since = 0;
    while (*sig >= '0' && *sig <= '9') since = since * 10 + (uint32_t)(*sig++ - '0');
    uint32_t version = wl_proxy_get_version(proxy);
    if (since > version) return 0;
    union wl_argument args[FUZZ_ARGS];
    struct wl_array arrays[FUZZ_ARGS];
    char strings[FUZZ_ARGS][24];
    unsigned char payload[FUZZ_ARGS][64];
    int fds[FUZZ_ARGS];
    unsigned n = 0, nfds = 0;
    int nullable = 0;
    const struct wl_interface *created = NULL;
    memset(args, 0, sizeof args);
    for (; *sig; sig++) {
        if (*sig == '?') { nullable = 1; continue; }
        if (n >= FUZZ_ARGS) goto skip;
        const struct wl_interface *type = message->types[n];
        switch (*sig) {
        case 'i': args[n].i = fuzz_int(f); break;
        case 'u': args[n].u = (uint32_t)fuzz_int(f); break;
        case 'f': args[n].f = wl_fixed_from_int(fuzz_int(f) % 5000); break;
        case 's':
            if (nullable && !fuzz_pick(f, 4)) { args[n].s = NULL; break; }
            for (unsigned k = 0, len = fuzz_pick(f, 23); k <= len; k++)
                strings[n][k] = k == len ? '\0' : (char)('a' + fuzz_pick(f, 26));
            args[n].s = strings[n];
            break;
        case 'o': {
            struct wl_proxy *target = NULL;
            if (!nullable || fuzz_pick(f, 4)) {
                /* An object of the declared type; rarely any object. */
                unsigned start = fuzz_pick(f, f->count);
                for (unsigned k = 0; k < f->count && !target; k++) {
                    unsigned j = (start + k) % f->count;
                    if (!type || f->iface[j] == type) target = f->live[j];
                }
                if (!fuzz_pick(f, 16)) target = f->live[start];
                /* Without one, usually try another request instead. */
                if (!target && fuzz_pick(f, 16)) goto skip;
                if (!target) target = f->live[start];
            }
            args[n].o = (struct wl_object *)target;
            break;
        }
        case 'n':
            if (!type || created) goto skip;
            created = type;
            args[n].n = 0;
            break;
        case 'a': {
            unsigned len = fuzz_pick(f, 64);
            for (unsigned k = 0; k < len; k++) payload[n][k] = (unsigned char)fuzz_next(f);
            arrays[n] = (struct wl_array){ .size = len, .alloc = len, .data = payload[n] };
            args[n].a = &arrays[n];
            break;
        }
        case 'h': {
            int fd = filled_memfd(4096, (unsigned char)fuzz_next(f));
            if (fd < 0) goto skip;
            fds[nfds++] = fd;
            args[n].h = fd;
            break;
        }
        default: goto skip;
        }
        nullable = 0;
        n++;
    }
    /* Destructors are sent without destroying the proxy: later requests
     * then name objects the server already destroyed. */
    struct wl_proxy *made = wl_proxy_marshal_array_flags(proxy, opcode, created, version, 0, args);
    for (unsigned k = 0; k < nfds; k++) close(fds[k]);
    if (made && f->count < FUZZ_LIVE) {
        f->live[f->count] = made;
        f->iface[f->count++] = created;
    }
    return 1;
skip:
    for (unsigned k = 0; k < nfds; k++) close(fds[k]);
    return 0;
}
static int fuzz(const char *mode, uint64_t seed) {
    unsigned requests = 0, rejected = 0, dropped = 0, survived = 0;
    for (unsigned session = 0; session < FUZZ_SESSIONS; session++) {
        struct fuzz f = { .display = wl_display_connect(NULL),
                          .rng = (seed * FUZZ_SESSIONS + session) * 2654435761u | 1 };
        if (!f.display) { fprintf(stderr, "%s: session %u could not connect\n", mode, session); return 1; }
        wl_registry_add_listener(wl_display_get_registry(f.display), &fuzz_registry, &f);
        if (bounded_roundtrip(f.display, 5000) < 0 || !f.count) {
            fprintf(stderr, "%s: session %u got no globals\n", mode, session); return 1;
        }
        /* errno says why a session ended: ETIMEDOUT only when the server
         * stopped answering; a closed socket (EPIPE without a display
         * error) is a disconnect. */
        int lost = 0;
        errno = 0;
        for (unsigned step = 0; step < FUZZ_STEPS && !lost; step++) {
            requests += (unsigned)fuzz_request(&f);
            lost = step % 8 == 7 && drain(f.display) < 0;
        }
        int outcome = lost ? -1 : bounded_roundtrip(f.display, 5000);
        int why = errno;
        if (outcome == 0) survived++;
        else if (wl_display_get_error(f.display) == EPROTO) rejected++;
        else if (why != ETIMEDOUT) dropped++;
        else {
            fprintf(stderr, "%s: seed %llu session %u: the server stopped answering\n", mode,
                    (unsigned long long)seed, session);
            return 1;
        }
        wl_display_disconnect(f.display);
    }
    printf("{\"mode\":\"%s\",\"result\":\"fuzz-served\",\"seed\":%llu,\"sessions\":%u,\"requests\":%u,"
           "\"protocol_errors\":%u,\"disconnected\":%u,\"survived\":%u}\n", mode, (unsigned long long)seed,
           (unsigned)FUZZ_SESSIONS, requests, rejected, dropped, survived);
    return 0;
}
int main(int argc, char **argv) {
    if (argc != 2 && !(argc == 3 && !strcmp(argv[1], "fuzz"))) {
        fprintf(stderr, "usage: tuna-wayland-adversary MODE | tuna-wayland-adversary fuzz SEED\n"); return 2;
    }
    const char *mode = argv[1];
    struct client c = { .display = wl_display_connect(NULL) };
    if (!c.display) { perror("wl_display_connect"); return 1; }
    struct wl_registry *registry = wl_display_get_registry(c.display);
    wl_registry_add_listener(registry, &registry_listener, &c);
    if (wl_display_roundtrip(c.display) < 0 || !c.compositor || !c.wm || !c.shm) {
        fprintf(stderr, "required globals unavailable\n"); return 1;
    }
    int result = 1;
    if (!strcmp(mode, "malformed-opcode")) {
        uint32_t header[2] = { 1, (8U << 16) | 0xffffU };
        if (write(wl_display_get_fd(c.display), header, sizeof header) != sizeof header) {
            perror("write malformed request"); return 1;
        }
        int roundtrip = wl_display_roundtrip(c.display);
        int error = wl_display_get_error(c.display);
        if (roundtrip >= 0 || (error != EPIPE && error != ECONNRESET && error != EPROTO)) {
            fprintf(stderr, "malformed request was not rejected: roundtrip=%d error=%d\n", roundtrip, error);
        } else if (error == EPROTO) {
            const struct wl_interface *interface = NULL;
            uint32_t object = 0;
            uint32_t code = wl_display_get_protocol_error(c.display, &interface, &object);
            if (interface && !strcmp(interface->name, "wl_display") && object == 1
                && code == WL_DISPLAY_ERROR_INVALID_METHOD) {
                printf("{\"mode\":\"%s\",\"result\":\"connection-rejected\",\"protocol_error\":%u}\n", mode, code);
                result = 0;
            } else fprintf(stderr, "malformed request received an unexpected protocol error\n");
        } else {
            /* Invalid wire opcodes may close transport before an error event. */
            printf("{\"mode\":\"%s\",\"result\":\"connection-rejected\",\"transport_error\":%d}\n", mode, error);
            result = 0;
        }
    } else if (!strcmp(mode, "fuzz")) {
        char *end = NULL;
        unsigned long long seed = strtoull(argv[2], &end, 10);
        if (!end || *end) { fprintf(stderr, "fuzz: SEED must be a decimal integer\n"); return 2; }
        result = fuzz(mode, seed);
        /* The long-lived connection is served after every fuzz session. */
        if (!result && bounded_roundtrip(c.display, 5000) < 0) {
            fprintf(stderr, "%s: existing connection lost during fuzzing\n", mode); result = 1;
        }
    } else if (!strcmp(mode, "bad-region")) {
        /* Found by the fuzzer: negative region rectangles and surface
         * damage panicked the compositor. Mutter's (pixman's) regions ignore
         * empty, negative and overflowing rectangles; the client stays
         * connected. */
        struct wl_region *region = wl_compositor_create_region(c.compositor);
        wl_region_add(region, 0, 0, -1252404857, 2);
        wl_region_add(region, 10, 10, 0, 0);
        wl_region_subtract(region, 0, 0, 5, -1);
        wl_region_add(region, INT_MAX, INT_MAX, 2, 2);
        wl_region_subtract(region, INT_MIN, 0, INT_MAX, INT_MAX);
        struct wl_surface *surface = wl_compositor_create_surface(c.compositor);
        wl_surface_set_input_region(surface, region);
        wl_surface_set_opaque_region(surface, region);
        wl_surface_damage(surface, 0, 0, 4, INT_MIN);
        wl_surface_commit(surface);
        wl_region_destroy(region);
        if (bounded_roundtrip(c.display, 5000) < 0) {
            fprintf(stderr, "%s: connection lost (%s)\n", mode, strerror(errno)); return 1;
        }
        printf("{\"mode\":\"%s\",\"result\":\"region-ignored\"}\n", mode);
        result = 0;
    } else if (!strcmp(mode, "connect-storm")) {
        result = connect_storm(mode);
        /* The long-lived connection must still be served after the storm. */
        if (!result && bounded_roundtrip(c.display, 5000) < 0) {
            fprintf(stderr, "%s: existing connection lost during the storm\n", mode); result = 1;
        }
    } else if (!strcmp(mode, "zero-buffer") || !strcmp(mode, "huge-buffer")) {
        char path[] = "/tmp/tuna-adversary-shm-XXXXXX";
        int fd = mkstemp(path);
        if (fd < 0) { perror("mkstemp"); return 1; }
        unlink(path);
        if (ftruncate(fd, 4096) < 0) { perror("ftruncate"); close(fd); return 1; }
        struct wl_shm_pool *pool = wl_shm_create_pool(c.shm, fd, 4096);
        close(fd);
        int width = !strcmp(mode, "zero-buffer") ? 0 : INT_MAX;
        (void)wl_shm_pool_create_buffer(pool, 0, width, 16, 64, WL_SHM_FORMAT_XRGB8888);
        result = require_error(&c, mode, "wl_shm_pool", WL_SHM_ERROR_INVALID_STRIDE);
    } else {
        struct wl_surface *surface = wl_compositor_create_surface(c.compositor);
        struct xdg_surface *xdg = xdg_wm_base_get_xdg_surface(c.wm, surface);
        xdg_surface_add_listener(xdg, &surface_listener, &c);
        struct xdg_toplevel *top = xdg_surface_get_toplevel(xdg);
        xdg_toplevel_add_listener(top, &top_listener, &c);
        if (!strcmp(mode, "commit-flood")) {
            result = commit_flood(&c, mode, surface, xdg);
        } else if (!strcmp(mode, "buffer-churn")) {
            result = buffer_churn(&c, mode, surface, xdg);
        } else if (!strcmp(mode, "die-mid-transaction")) {
            /* Deliver the new role, then disconnect before its initial commit. */
            if (wl_display_roundtrip(c.display) < 0) return 1;
            printf("{\"mode\":\"%s\",\"result\":\"disconnected-before-commit\"}\n", mode);
            result = 0;
        } else {
            wl_surface_commit(surface);
            for (unsigned i = 0; i < 4 && !c.configured; i++)
                if (wl_display_roundtrip(c.display) < 0) return 1;
            if (!c.configured) { fprintf(stderr, "no actual initial configure\n"); return 1; }
            if (!strcmp(mode, "destroy-parent")) {
                /* Send the destructor but retain its proxy for error identity. */
                wl_proxy_marshal_flags((struct wl_proxy *)c.wm, XDG_WM_BASE_DESTROY, NULL,
                                       wl_proxy_get_version((struct wl_proxy *)c.wm), 0);
                result = require_error(&c, mode, "xdg_wm_base", XDG_WM_BASE_ERROR_DEFUNCT_SURFACES);
            } else if (!strcmp(mode, "invalid-configure")) {
                xdg_surface_ack_configure(xdg, UINT32_MAX);
                result = require_error(&c, mode, "xdg_wm_base", XDG_WM_BASE_ERROR_INVALID_SURFACE_STATE);
            } else if (!strcmp(mode, "never-ack")) {
                /* A client may remain unmapped without blocking others. */
                if (wl_display_roundtrip(c.display) < 0) return 1;
                sleep(1);
                printf("{\"mode\":\"%s\",\"result\":\"configure-left-unacknowledged\",\"configures\":%u}\n", mode, c.configured);
                result = 0;
            } else { fprintf(stderr, "unknown mode: %s\n", mode); result = 2; }
        }
    }
    wl_display_disconnect(c.display);
    return result;
}
