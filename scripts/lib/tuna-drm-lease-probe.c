/* CI-only native client for wp_drm_lease_device_v1 (drm-smoke D-LEASE).
 *
 *   tuna-drm-lease-probe                  list every lease device and its
 *                                         connectors as one JSON line
 *   tuna-drm-lease-probe --lease NAME     also lease connector NAME, check
 *                                         the lessee fd sees exactly it,
 *                                         that it was withdrawn, then end
 *                                         the lease by destroying it
 *   ... --lease NAME --close-fd           end it by closing the lease fd
 *                                         instead (the compositor must send
 *                                         finished)
 *   ... --lease NAME --disconnect         disconnect the owning Wayland
 *                                         client while retaining its lease FD;
 *                                         a new client must see the connector
 *                                         and the old FD must be revoked
 *   ... --lease NAME --stale              submit a request made before
 *                                         withdrawal; it must receive finished
 *   ... --lease NAME --duplicate          require duplicate_connector error
 *   tuna-drm-lease-probe --empty          require empty_lease error
 *   ... --lease NAME --expect-revoke     wait for actual connector unplug,
 *                                         requiring finished and revoked FD
 *   ... --observe NAME                   independently observe withdrawal and
 *                                         re-offering while another client leases
 * Either way the connector must be offered again. Exits nonzero on any
 * protocol step that does not happen within its deadline. */
#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <xf86drm.h>
#include <drm_mode.h>
#include <wayland-client.h>
#include "drm-lease-v1-client-protocol.h"

#define MAX_DEVICES 4
#define MAX_CONNECTORS 32
#define DEADLINE_MS 5000

struct connector {
    struct wp_drm_lease_connector_v1 *proxy;
    char name[64];
    char description[128];
    uint32_t id;
    bool done, withdrawn;
};
struct device {
    struct wp_drm_lease_device_v1 *proxy;
    int drm_fd;
    uint32_t version;
    struct connector connectors[MAX_CONNECTORS];
    unsigned n_connectors;
    unsigned dones;
    bool released;
};
static struct wl_display *display;
static struct device devices[MAX_DEVICES];
static unsigned n_devices;
static int lease_fd = -1;
static bool lease_finished;
static const char *observe_name;
static bool observed_withdrawn;
static bool observed_reoffered;
static bool stale_finished;
static bool stale_fd;

static double now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

/* Dispatch until *flag holds or the deadline passes. */
static bool dispatch_until(const bool *flag) {
    double end = now_ms() + DEADLINE_MS;
    while (!*flag) {
        if (wl_display_dispatch_pending(display) < 0) return false;
        if (*flag) break;
        double left = end - now_ms();
        if (left <= 0) return false;
        while (wl_display_prepare_read(display) != 0) wl_display_dispatch_pending(display);
        wl_display_flush(display);
        struct pollfd pfd = { .fd = wl_display_get_fd(display), .events = POLLIN };
        int r = poll(&pfd, 1, (int)left);
        if (r <= 0) { wl_display_cancel_read(display); if (r < 0 && errno != EINTR) return false; continue; }
        if (wl_display_read_events(display) < 0) return false;
    }
    return true;
}

static void connector_name(void *data, struct wp_drm_lease_connector_v1 *c, const char *name) {
    (void)c; snprintf(((struct connector *)data)->name, sizeof ((struct connector *)data)->name, "%s", name);
}
static void connector_description(void *data, struct wp_drm_lease_connector_v1 *c, const char *d) {
    (void)c; snprintf(((struct connector *)data)->description, sizeof ((struct connector *)data)->description, "%s", d);
}
static void connector_id(void *data, struct wp_drm_lease_connector_v1 *c, uint32_t id) {
    (void)c; ((struct connector *)data)->id = id;
}
static void connector_done(void *data, struct wp_drm_lease_connector_v1 *c) {
    (void)c;
    struct connector *conn = data;
    conn->done = true;
    if (observe_name && observed_withdrawn && !conn->withdrawn &&
        strcmp(conn->name, observe_name) == 0) {
        observed_reoffered = true;
        printf("{\"event\":\"observer-reoffered\",\"id\":%u}\n", conn->id);
        fflush(stdout);
    }
}
static void connector_withdrawn(void *data, struct wp_drm_lease_connector_v1 *c) {
    (void)c;
    struct connector *conn = data;
    conn->withdrawn = true;
    if (observe_name && strcmp(conn->name, observe_name) == 0) {
        observed_withdrawn = true;
        printf("{\"event\":\"observer-withdrawn\",\"id\":%u}\n", conn->id);
        fflush(stdout);
    }
}
static const struct wp_drm_lease_connector_v1_listener connector_listener = {
    .name = connector_name, .description = connector_description,
    .connector_id = connector_id, .done = connector_done, .withdrawn = connector_withdrawn,
};

static void device_drm_fd(void *data, struct wp_drm_lease_device_v1 *d, int32_t fd) {
    (void)d; ((struct device *)data)->drm_fd = fd;
}
static void device_connector(void *data, struct wp_drm_lease_device_v1 *d, struct wp_drm_lease_connector_v1 *c) {
    (void)d;
    struct device *dev = data;
    if (dev->n_connectors == MAX_CONNECTORS) { fprintf(stderr, "too many connectors\n"); exit(1); }
    struct connector *conn = &dev->connectors[dev->n_connectors++];
    memset(conn, 0, sizeof *conn);
    conn->proxy = c;
    wp_drm_lease_connector_v1_add_listener(c, &connector_listener, conn);
}
static void device_done(void *data, struct wp_drm_lease_device_v1 *d) {
    (void)d;
    struct device *dev = data;
    for (unsigned i = 0; i < dev->n_connectors; ++i) {
        struct connector *conn = &dev->connectors[i];
        if (!conn->withdrawn && (!conn->done || !conn->id || !conn->name[0])) {
            fprintf(stderr, "lease device done preceded live connector metadata/done\n");
            exit(1);
        }
    }
    dev->dones++;
}
static void device_released(void *data, struct wp_drm_lease_device_v1 *d) {
    (void)d; ((struct device *)data)->released = true;
}
static const struct wp_drm_lease_device_v1_listener device_listener = {
    .drm_fd = device_drm_fd, .connector = device_connector, .done = device_done, .released = device_released,
};

static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
    (void)data;
    if (strcmp(interface, wp_drm_lease_device_v1_interface.name) != 0 || n_devices == MAX_DEVICES) return;
    struct device *dev = &devices[n_devices++];
    dev->drm_fd = -1;
    dev->version = version;
    dev->proxy = wl_registry_bind(registry, name, &wp_drm_lease_device_v1_interface, 1);
    wp_drm_lease_device_v1_add_listener(dev->proxy, &device_listener, dev);
}
static void global_remove(void *data, struct wl_registry *registry, uint32_t name) {
    (void)data; (void)registry; (void)name;
}
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = global_remove };

static void lease_fd_event(void *data, struct wp_drm_lease_v1 *l, int32_t fd) {
    (void)data; (void)l; lease_fd = fd;
}
static void lease_finished_event(void *data, struct wp_drm_lease_v1 *l) {
    (void)data; (void)l; lease_finished = true;
}
static const struct wp_drm_lease_v1_listener lease_listener = { .lease_fd = lease_fd_event, .finished = lease_finished_event };

static void stale_fd_event(void *data, struct wp_drm_lease_v1 *l, int32_t fd) {
    (void)data; (void)l;
    stale_fd = true;
    close(fd);
}
static void stale_finished_event(void *data, struct wp_drm_lease_v1 *l) {
    (void)data; (void)l; stale_finished = true;
}
static const struct wp_drm_lease_v1_listener stale_listener = {
    .lease_fd = stale_fd_event, .finished = stale_finished_event,
};

static void json_string(const char *s) {
    putchar('"');
    for (; *s; s++) {
        if (*s == '"' || *s == '\\') printf("\\%c", *s);
        else if ((unsigned char)*s < 0x20) printf("\\u%04x", *s);
        else putchar(*s);
    }
    putchar('"');
}

/* Live (not withdrawn) connectors of the device, as JSON. */
static void print_devices(const char *event) {
    printf("{\"event\":\"%s\",\"devices\":[", event);
    for (unsigned i = 0; i < n_devices; i++) {
        struct device *dev = &devices[i];
        struct stat st;
        bool drm = dev->drm_fd >= 0 && fstat(dev->drm_fd, &st) == 0 && S_ISCHR(st.st_mode);
        printf("%s{\"version\":%u,\"drm_fd\":%s,\"released\":%s,\"connectors\":[", i ? "," : "", dev->version,
               drm ? "true" : "false", dev->released ? "true" : "false");
        bool first = true;
        for (unsigned j = 0; j < dev->n_connectors; j++) {
            struct connector *c = &dev->connectors[j];
            if (c->withdrawn) continue;
            printf("%s{\"name\":", first ? "" : ",");
            json_string(c->name);
            printf(",\"description\":");
            json_string(c->description);
            printf(",\"id\":%u,\"done\":%s}", c->id, c->done ? "true" : "false");
            first = false;
        }
        printf("]}");
    }
    printf("]}\n");
    fflush(stdout);
}

static struct connector *find_live(struct device *dev, const char *name) {
    for (unsigned j = dev->n_connectors; j-- > 0;) {
        struct connector *c = &dev->connectors[j];
        if (!c->withdrawn && strcmp(c->name, name) == 0) return c;
    }
    return NULL;
}

static int fail(const char *what) {
    fprintf(stderr, "tuna-drm-lease-probe: %s\n", what);
    return 1;
}

static int connect_client(void) {
    display = wl_display_connect(NULL);
    if (!display) return fail("cannot connect to the Wayland display");
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    if (wl_display_roundtrip(display) < 0) return fail("registry roundtrip failed");
    for (unsigned i = 0; i < n_devices; i++) {
        bool done = false;
        double end = now_ms() + DEADLINE_MS;
        while (!(done = devices[i].dones > 0 || devices[i].released) && now_ms() < end)
            if (wl_display_roundtrip(display) < 0) return fail("device roundtrip failed");
        if (!done) return fail("a lease device never sent done");
        if (devices[i].drm_fd >= 0 && drmIsMaster(devices[i].drm_fd) != 0)
            return fail("lease device leaked a DRM master FD");
    }
    return 0;
}

static uint32_t wire_word(const unsigned char *bytes) {
    uint32_t value;
    memcpy(&value, bytes, sizeof(value));
    return value;
}

static int expect_request_error(uint32_t request_id, uint32_t expected) {
    /* submit is a destructor: libwayland has already retired the request's
     * proxy when its error arrives, so its cached interface can be NULL.
     * Capture the typed request's ID before submit and inspect the actual
     * display.error instead. No roundtrip may consume that event first.
     * EOF, I/O failures and errors targeting another object are not evidence. */
    unsigned char bytes[65536];
    size_t used = 0;
    int fd = wl_display_get_fd(display);
    if (wl_display_flush(display) < 0) return fail("cannot flush invalid lease request");
    double end = now_ms() + DEADLINE_MS;
    for (;;) {
        double remaining = end - now_ms();
        if (remaining <= 0) break;
        struct pollfd pfd = { .fd = fd, .events = POLLIN };
        int ready = poll(&pfd, 1, (int)remaining);
        if (ready < 0 && errno == EINTR) continue;
        if (ready < 0) return fail("cannot poll lease request error");
        if (ready == 0) break;
        ssize_t count = recv(fd, bytes + used, sizeof(bytes) - used, MSG_DONTWAIT);
        if (count < 0 && (errno == EINTR || errno == EAGAIN)) continue;
        if (count < 0) return fail("cannot read lease request error");
        if (count == 0) return fail("lease request closed without display.error");
        used += (size_t)count;
        size_t offset = 0;
        while (used - offset >= 8) {
            const unsigned char *frame = bytes + offset;
            uint32_t header = wire_word(frame + 4);
            uint32_t size = header >> 16;
            uint32_t opcode = header & 0xffff;
            if (size < 8 || size % 4 != 0)
                return fail("malformed lease request error frame");
            if (size > used - offset) break;
            if (wire_word(frame) != 1)
                return fail("unexpected object event before lease request error");
            if (opcode == 0) { /* wl_display.error(object_id, code, message) */
                if (size < 24) return fail("truncated lease request error");
                uint32_t object_id = wire_word(frame + 8);
                uint32_t code = wire_word(frame + 12);
                uint32_t length = wire_word(frame + 16);
                if (length < 2 || length > size - 20 ||
                    20 + ((length + 3) & ~3u) != size ||
                    (const unsigned char *)memchr(frame + 20, '\0', length) != frame + 20 + length - 1)
                    return fail("malformed lease request error message");
                if (object_id != request_id || code != expected)
                    return fail("wrong lease request protocol error target or code");
                printf("{\"event\":\"request-error\",\"object_id\":%u,\"code\":%u,\"message\":",
                       object_id, code);
                json_string((const char *)frame + 20);
                printf("}\n");
                wl_display_disconnect(display);
                return 0;
            }
            /* A completed initial roundtrip can leave its callback's
             * wl_display.delete_id in flight. It is not an error receipt. */
            if (opcode != 1 || size != 12 ||
                wire_word(frame + 8) < 2 || wire_word(frame + 8) >= 0xff000000u)
                return fail("unexpected display event before lease request error");
            offset += size;
        }
        if (offset != 0) {
            used -= offset;
            memmove(bytes, bytes + offset, used);
        }
        if (used == sizeof(bytes)) return fail("oversized lease request error frame");
    }
    return fail("invalid lease request did not produce a protocol error");
}

static bool lease_revoked(int fd, struct drm_mode_card_res *visible) {
    struct drm_mode_get_lease authority = { 0 };
    if (ioctl(fd, DRM_IOCTL_MODE_GET_LEASE, &authority) != 0 ||
        ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, visible) != 0) {
        fprintf(stderr, "tuna-drm-lease-probe: native revocation ioctl failed: %s\n", strerror(errno));
        return false;
    }
    /* GETLEASE enumerates actual held authority, including planes.
     * GETRESOURCES filters CRTCs/connectors by that lease, but its encoder
     * list is device-global even after revocation (Linux drm_mode_getresources).
     * Global encoder visibility is not permission to use a leased object. */
    fprintf(stderr, "tuna-drm-lease-probe: revoked FD held_objects=%u connectors=%u crtcs=%u global_encoders=%u\n",
            authority.count_objects, visible->count_connectors, visible->count_crtcs, visible->count_encoders);
    return authority.count_objects == 0 && visible->count_connectors == 0 && visible->count_crtcs == 0;
}

int main(int argc, char **argv) {
    const char *lease_name = NULL;
    const char *await_name = NULL;
    bool close_fd = false, disconnect_owner = false, empty = false;
    bool duplicate = false, stale = false, expect_revoke = false;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--lease") == 0 && i + 1 < argc) lease_name = argv[++i];
        else if (strcmp(argv[i], "--observe") == 0 && i + 1 < argc) observe_name = argv[++i];
        else if (strcmp(argv[i], "--await") == 0 && i + 1 < argc) await_name = argv[++i];
        else if (strcmp(argv[i], "--close-fd") == 0) close_fd = true;
        else if (strcmp(argv[i], "--disconnect") == 0) disconnect_owner = true;
        else if (strcmp(argv[i], "--duplicate") == 0) duplicate = true;
        else if (strcmp(argv[i], "--stale") == 0) stale = true;
        else if (strcmp(argv[i], "--expect-revoke") == 0) expect_revoke = true;
        else if (strcmp(argv[i], "--empty") == 0) empty = true;
        else return fail("unknown option or missing connector name");
    }
    if (connect_client() != 0) return 1;
    if (await_name) {
        double end = now_ms() + DEADLINE_MS;
        while ((!n_devices || !find_live(&devices[0], await_name)) && now_ms() < end)
            if (wl_display_roundtrip(display) < 0) return fail("connector reappearance roundtrip failed");
        if (!n_devices || !find_live(&devices[0], await_name))
            return fail("restored connector was not offered");
        print_devices("available");
        wl_display_disconnect(display);
        return 0;
    }
    print_devices("devices");
    if (observe_name) {
        struct connector *target = n_devices ? find_live(&devices[0], observe_name) : NULL;
        if (!target) return fail("observer target was not offered");
        if (!dispatch_until(&observed_withdrawn) || !dispatch_until(&observed_reoffered))
            return fail("independent client did not observe withdrawal and re-offering");
        if (!find_live(&devices[0], observe_name))
            return fail("independent client has no re-offered connector");
        print_devices("observer-complete");
        wl_display_disconnect(display);
        return 0;
    }
    if (empty) {
        if (!n_devices) return fail("no lease device for empty request");
        struct wp_drm_lease_request_v1 *request = wp_drm_lease_device_v1_create_lease_request(devices[0].proxy);
        uint32_t request_id = wl_proxy_get_id((struct wl_proxy *)request);
        wp_drm_lease_request_v1_submit(request);
        return expect_request_error(request_id, WP_DRM_LEASE_REQUEST_V1_ERROR_EMPTY_LEASE);
    }
    if (!lease_name) return 0;

    if (n_devices == 0) return fail("no lease device");
    struct device *dev = &devices[0];
    struct connector *target = find_live(dev, lease_name);
    if (!target) return fail("the connector to lease is not offered");
    uint32_t target_id = target->id;
    unsigned dones = dev->dones;
    struct wp_drm_lease_request_v1 *stale_request = NULL;
    if (stale) {
        stale_request = wp_drm_lease_device_v1_create_lease_request(dev->proxy);
        wp_drm_lease_request_v1_request_connector(stale_request, target->proxy);
    }
    struct wp_drm_lease_request_v1 *request = wp_drm_lease_device_v1_create_lease_request(dev->proxy);
    wp_drm_lease_request_v1_request_connector(request, target->proxy);
    if (duplicate) {
        uint32_t request_id = wl_proxy_get_id((struct wl_proxy *)request);
        wp_drm_lease_request_v1_request_connector(request, target->proxy);
        return expect_request_error(request_id, WP_DRM_LEASE_REQUEST_V1_ERROR_DUPLICATE_CONNECTOR);
    }
    struct wp_drm_lease_v1 *lease = wp_drm_lease_request_v1_submit(request);
    wp_drm_lease_v1_add_listener(lease, &lease_listener, NULL);
    bool leased = false;
    double end = now_ms() + DEADLINE_MS;
    while (!(leased = lease_fd >= 0) && !lease_finished && now_ms() < end)
        if (wl_display_roundtrip(display) < 0) return fail("lease roundtrip failed");
    if (!leased) return fail(lease_finished ? "the lease was refused (finished)" : "no lease_fd");
    if (drmIsMaster(lease_fd) != 1) return fail("the lease FD is not a DRM lessee master");
    struct drm_mode_get_lease authority = { 0 };
    if (ioctl(lease_fd, DRM_IOCTL_MODE_GET_LEASE, &authority) != 0 || authority.count_objects == 0)
        return fail("the lease FD does not hold native lease authority");

    /* The lessee's fd shows exactly the leased connector and one CRTC. */
    struct drm_mode_card_res res = { 0 };
    if (ioctl(lease_fd, DRM_IOCTL_MODE_GETRESOURCES, &res) != 0) return fail("GETRESOURCES on the lease fd failed");
    uint32_t connectors[MAX_CONNECTORS] = { 0 };
    if (res.count_connectors > MAX_CONNECTORS) return fail("lease fd lists too many connectors");
    uint32_t n_crtcs = res.count_crtcs;
    struct drm_mode_card_res ids = { 0 };
    ids.count_connectors = res.count_connectors;
    ids.connector_id_ptr = (uint64_t)(uintptr_t)connectors;
    if (ioctl(lease_fd, DRM_IOCTL_MODE_GETRESOURCES, &ids) != 0) return fail("GETRESOURCES (ids) failed");
    printf("{\"event\":\"leased\",\"connector\":");
    json_string(lease_name);
    printf(",\"lessee_connectors\":[");
    for (uint32_t i = 0; i < res.count_connectors; i++) printf("%s%u", i ? "," : "", connectors[i]);
    printf("],\"lessee_crtcs\":%u,\"lessee_objects\":%u}\n", n_crtcs, authority.count_objects);
    fflush(stdout);
    if (res.count_connectors != 1 || connectors[0] != target_id || n_crtcs != 1)
        return fail("the lease fd does not hold exactly the leased connector and a CRTC");

    /* Leased: withdrawn from this client, then done. */
    end = now_ms() + DEADLINE_MS;
    while (!(target->withdrawn && dev->dones > dones) && now_ms() < end)
        if (wl_display_roundtrip(display) < 0) return fail("withdraw roundtrip failed");
    if (!target->withdrawn) return fail("the leased connector was not withdrawn");
    print_devices("withdrawn");
    if (expect_revoke) {
        if (!dispatch_until(&lease_finished))
            return fail("connector unplug did not finish its active lease");
        struct drm_mode_card_res revoked = { 0 };
        if (!lease_revoked(lease_fd, &revoked))
            return fail("connector unplug did not revoke the native lease FD");
        /* Drain the withdrawal batch before inspecting available objects. */
        if (wl_display_roundtrip(display) < 0) return fail("unplug roundtrip failed");
        if (find_live(dev, lease_name)) return fail("unplugged connector is still offered");
        printf("{\"event\":\"unplug-revoked\",\"held_objects\":0,\"global_encoders\":%u}\n",
               revoked.count_encoders);
        fflush(stdout);
        close(lease_fd);
        wp_drm_lease_v1_destroy(lease);
        wl_display_disconnect(display);
        return 0;
    }
    if (stale) {
        struct wp_drm_lease_v1 *refused = wp_drm_lease_request_v1_submit(stale_request);
        wp_drm_lease_v1_add_listener(refused, &stale_listener, NULL);
        if (!dispatch_until(&stale_finished) || stale_fd)
            return fail("a stale connector request was not refused with finished");
        printf("{\"event\":\"stale-finished\"}\n");
        wp_drm_lease_v1_destroy(refused);
    }
    if (disconnect_owner) {
        /* Keep the native FD open: revocation must follow Wayland owner loss,
         * not merely the kernel noticing that the final FD was closed. */
        wl_display_disconnect(display);
        for (unsigned i = 0; i < n_devices; i++)
            if (devices[i].drm_fd >= 0) close(devices[i].drm_fd);
        memset(devices, 0, sizeof devices);
        n_devices = 0;
        if (connect_client() != 0) return 1;
        if (!n_devices || !find_live(&devices[0], lease_name))
            return fail("owner exit did not re-offer its connector to a new client");
        struct drm_mode_card_res revoked = { 0 };
        if (!lease_revoked(lease_fd, &revoked))
            return fail("Wayland owner exit did not revoke the still-open lease FD");
        printf("{\"event\":\"owner-revoked\",\"held_objects\":0,\"global_encoders\":%u}\n",
               revoked.count_encoders);
        print_devices("reoffered");
        close(lease_fd);
        wl_display_disconnect(display);
        return 0;
    }

    dones = dev->dones;
    if (close_fd) {
        /* The kernel destroys the lessee; the compositor ends the lease. */
        close(lease_fd);
        if (!dispatch_until(&lease_finished)) return fail("closing the lease fd did not finish the lease");
        printf("{\"event\":\"finished\"}\n");
        fflush(stdout);
    }
    wp_drm_lease_v1_destroy(lease);
    /* Offered again: a new connector object with the same name, then done. */
    end = now_ms() + DEADLINE_MS;
    while (!(find_live(dev, lease_name) && dev->dones > dones) && now_ms() < end)
        if (wl_display_roundtrip(display) < 0) return fail("re-offer roundtrip failed");
    if (!find_live(dev, lease_name)) return fail("the connector was not offered again");
    print_devices("reoffered");
    wl_display_disconnect(display);
    return 0;
}
