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
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <drm/drm.h>
#include <drm/drm_mode.h>
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
    (void)c; ((struct connector *)data)->done = true;
}
static void connector_withdrawn(void *data, struct wp_drm_lease_connector_v1 *c) {
    (void)c; ((struct connector *)data)->withdrawn = true;
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
    (void)d; ((struct device *)data)->dones++;
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

int main(int argc, char **argv) {
    const char *lease_name = NULL;
    bool close_fd = false;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--lease") == 0 && i + 1 < argc) lease_name = argv[++i];
        else if (strcmp(argv[i], "--close-fd") == 0) close_fd = true;
        else return fail("usage: tuna-drm-lease-probe [--lease NAME [--close-fd]]");
    }
    display = wl_display_connect(NULL);
    if (!display) return fail("cannot connect to the Wayland display");
    struct wl_registry *registry = wl_display_get_registry(display);
    wl_registry_add_listener(registry, &registry_listener, NULL);
    if (wl_display_roundtrip(display) < 0) return fail("registry roundtrip failed");
    /* Every bound device sends drm_fd, its connectors, then done. */
    for (unsigned i = 0; i < n_devices; i++) {
        bool done = false;
        double end = now_ms() + DEADLINE_MS;
        while (!(done = devices[i].dones > 0 || devices[i].released) && now_ms() < end)
            if (wl_display_roundtrip(display) < 0) return fail("device roundtrip failed");
        if (!done) return fail("a lease device never sent done");
    }
    print_devices("devices");
    if (!lease_name) return 0;

    if (n_devices == 0) return fail("no lease device");
    struct device *dev = &devices[0];
    struct connector *target = find_live(dev, lease_name);
    if (!target) return fail("the connector to lease is not offered");
    uint32_t target_id = target->id;
    unsigned dones = dev->dones;
    struct wp_drm_lease_request_v1 *request = wp_drm_lease_device_v1_create_lease_request(dev->proxy);
    wp_drm_lease_request_v1_request_connector(request, target->proxy);
    struct wp_drm_lease_v1 *lease = wp_drm_lease_request_v1_submit(request);
    wp_drm_lease_v1_add_listener(lease, &lease_listener, NULL);
    bool leased = false;
    double end = now_ms() + DEADLINE_MS;
    while (!(leased = lease_fd >= 0) && !lease_finished && now_ms() < end)
        if (wl_display_roundtrip(display) < 0) return fail("lease roundtrip failed");
    if (!leased) return fail(lease_finished ? "the lease was refused (finished)" : "no lease_fd");

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
    printf("],\"lessee_crtcs\":%u}\n", n_crtcs);
    fflush(stdout);
    if (res.count_connectors != 1 || connectors[0] != target_id || n_crtcs != 1)
        return fail("the lease fd does not hold exactly the leased connector and a CRTC");

    /* Leased: withdrawn from this client, then done. */
    end = now_ms() + DEADLINE_MS;
    while (!(target->withdrawn && dev->dones > dones) && now_ms() < end)
        if (wl_display_roundtrip(display) < 0) return fail("withdraw roundtrip failed");
    if (!target->withdrawn) return fail("the leased connector was not withdrawn");
    print_devices("withdrawn");

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
