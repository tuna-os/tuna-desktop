/* Native linux-drm-syncobj + dmabuf-feedback consumer. No fabricated DRM fd,
 * CPU-only buffer, software stand-in, capability override or retry path.
 * Build with scripts/tuna-explicit-sync-build. Run the proof driver against an
 * already-running owned native compositor and a timeline-capable render node.
 * --checkpoints pauses at JSON checkpoints for independent display capture. */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <poll.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#include <xf86drm.h>
#include <drm_fourcc.h>
#include <gbm.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <wayland-client.h>
#include "xdg-shell-client-protocol.h"
#include "presentation-time-client-protocol.h"
#include "linux-dmabuf-client-protocol.h"
#include "linux-drm-syncobj-client-protocol.h"

#define SIDE 64
#define DEADLINE_MS 5000
#define MAX_FORMATS 4096
#define REQUIRE(test, text) do { if (!(test)) fail(text); } while (0)
struct format { uint32_t fourcc, padding; uint64_t modifier; };
struct offer { struct format format; dev_t target; uint32_t flags; };
struct feedback {
    const char *name;
    struct format *table;
    size_t table_bytes, table_count;
    dev_t main, target;
    uint32_t flags;
    bool main_seen, target_seen, done;
    struct offer offers[MAX_FORMATS];
    size_t count;
};
struct buffer {
    struct gbm_bo *bo;
    struct wl_buffer *wl;
    EGLImageKHR image;
    GLuint rb, fb;
    uint32_t acquire, release;
    struct wp_linux_drm_syncobj_timeline_v1 *acquire_wl, *release_wl;
};
struct presented { bool done, discarded; uint64_t sequence, ns; uint32_t flags; };
static struct wl_display *display;
static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm;
static struct wp_presentation *presentation;
static struct zwp_linux_dmabuf_v1 *dmabuf;
static struct wp_linux_drm_syncobj_manager_v1 *sync_manager;
static struct wl_surface *surface;
static struct wp_linux_drm_syncobj_surface_v1 *sync_surface;
static struct xdg_surface *xdg;
static struct xdg_toplevel *toplevel;
static struct feedback defaults = { .name = "default" }, local = { .name = "surface" };
static struct gbm_device *gbm;
static EGLDisplay egl_display;
static EGLContext egl_context;
static int drm_fd = -1;
static const char *artifacts;
static bool checkpoints, configured;
static dev_t render_device;
static PFNEGLCREATEIMAGEKHRPROC create_image;
static PFNEGLDESTROYIMAGEKHRPROC destroy_image;
static PFNEGLCREATESYNCKHRPROC create_sync;
static PFNEGLDESTROYSYNCKHRPROC destroy_sync;
static PFNEGLDUPNATIVEFENCEFDANDROIDPROC dup_fence;
static PFNGLEGLIMAGETARGETRENDERBUFFERSTORAGEOESPROC image_storage;

static void fail(const char *message) {
    fprintf(stderr, "native-explicit-sync: %s (errno=%d %s, EGL=%#x GL=%#x)\n",
            message, errno, strerror(errno), eglGetError(), glGetError());
    exit(1);
}
static int64_t now_ns(void) {
    struct timespec t; REQUIRE(clock_gettime(CLOCK_MONOTONIC, &t) == 0, "clock");
    return (int64_t)t.tv_sec * INT64_C(1000000000) + t.tv_nsec;
}
static bool dispatch_for(int milliseconds, const bool *done) {
    int64_t end = now_ns() + (int64_t)milliseconds * 1000000;
    while (!done || !*done) {
        if (wl_display_dispatch_pending(display) < 0) return false;
        if (done && *done) return true;
        int64_t left = end - now_ns();
        if (left <= 0) return !done;
        while (wl_display_prepare_read(display) != 0)
            if (wl_display_dispatch_pending(display) < 0) return false;
        if (wl_display_flush(display) < 0 && errno != EAGAIN) {
            wl_display_cancel_read(display); return false;
        }
        struct pollfd p = { .fd = wl_display_get_fd(display), .events = POLLIN };
        int n = poll(&p, 1, (int)((left + 999999) / 1000000));
        if (n < 0) { wl_display_cancel_read(display); if (errno == EINTR) continue; return false; }
        if (n == 0) { wl_display_cancel_read(display); return !done || *done; }
        if (wl_display_read_events(display) < 0) return false;
    }
    return true;
}
static bool same_device(dev_t named) {
    drmDevicePtr device = NULL;
    if (drmGetDeviceFromDevId(named, 0, &device) != 0) return false;
    bool same = false;
    if (device->available_nodes & (1 << DRM_NODE_RENDER)) {
        struct stat st;
        same = stat(device->nodes[DRM_NODE_RENDER], &st) == 0 && st.st_rdev == render_device;
    }
    drmFreeDevice(&device);
    return same;
}
static dev_t decode_device(struct wl_array *array) {
    REQUIRE(array->size == sizeof(dev_t), "feedback device has wrong native dev_t size");
    dev_t result; memcpy(&result, array->data, sizeof(result)); return result;
}
static void feedback_done(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy) {
    (void)proxy; struct feedback *f = data;
    REQUIRE(f->main_seen && f->table && f->count, "incomplete dmabuf feedback");
    f->done = true;
}
static void feedback_table(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy, int fd, uint32_t bytes) {
    (void)proxy; struct feedback *f = data; struct stat st;
    REQUIRE(!f->table && bytes && bytes % sizeof(struct format) == 0 &&
            bytes / sizeof(struct format) <= MAX_FORMATS && fstat(fd, &st) == 0 &&
            st.st_size >= bytes, "invalid feedback format-table fd/size");
    f->table = mmap(NULL, bytes, PROT_READ, MAP_PRIVATE, fd, 0); close(fd);
    REQUIRE(f->table != MAP_FAILED, "map actual feedback format-table");
    f->table_bytes = bytes; f->table_count = bytes / sizeof(struct format);
    char path[4096];
    REQUIRE(snprintf(path, sizeof(path), "%s/%s-format-table.bin", artifacts, f->name) < (int)sizeof(path), "feedback artifact path");
    FILE *out = fopen(path, "wb"); REQUIRE(out, "open actual feedback table artifact");
    REQUIRE(fwrite(f->table, 1, bytes, out) == bytes && fclose(out) == 0, "retain actual fd format-table bytes");
}
static void feedback_main(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy, struct wl_array *device) {
    (void)proxy; struct feedback *f = data; f->main = decode_device(device); f->main_seen = true;
}
static void tranche_done(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy) {
    (void)proxy; struct feedback *f = data; REQUIRE(f->target_seen, "tranche lacks target device");
    f->target_seen = false; f->flags = 0;
}
static void tranche_target(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy, struct wl_array *device) {
    (void)proxy; struct feedback *f = data; f->target = decode_device(device); f->target_seen = true;
}
static void tranche_formats(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy, struct wl_array *indices) {
    (void)proxy; struct feedback *f = data;
    REQUIRE(f->table && f->target_seen && indices->size % sizeof(uint16_t) == 0, "invalid tranche indexes");
    uint16_t *index;
    wl_array_for_each(index, indices) {
        REQUIRE(*index < f->table_count && f->count < MAX_FORMATS, "tranche index outside fd format table");
        f->offers[f->count++] = (struct offer){ f->table[*index], f->target, f->flags };
        printf("{\"event\":\"tranche-index\",\"feedback\":\"%s\",\"index\":%u,\"target_device\":%ju,\"flags\":%u,\"fourcc\":%u,\"modifier\":%" PRIu64 "}\n",
               f->name, *index, (uintmax_t)f->target, f->flags, f->table[*index].fourcc, f->table[*index].modifier);
    }
}
static void tranche_flags(void *data, struct zwp_linux_dmabuf_feedback_v1 *proxy, uint32_t flags) {
    (void)proxy; ((struct feedback *)data)->flags = flags;
}
static const struct zwp_linux_dmabuf_feedback_v1_listener feedback_listener = {
    .done = feedback_done, .format_table = feedback_table, .main_device = feedback_main,
    .tranche_done = tranche_done, .tranche_target_device = tranche_target,
    .tranche_formats = tranche_formats, .tranche_flags = tranche_flags,
};
static void ping(void *data, struct xdg_wm_base *proxy, uint32_t serial) { (void)data; xdg_wm_base_pong(proxy, serial); }
static const struct xdg_wm_base_listener wm_listener = { .ping = ping };
static void configure(void *data, struct xdg_surface *proxy, uint32_t serial) {
    (void)data; xdg_surface_ack_configure(proxy, serial); configured = true;
}
static const struct xdg_surface_listener xdg_listener = { .configure = configure };
static void global(void *data, struct wl_registry *registry, uint32_t name, const char *interface, uint32_t version) {
    (void)data;
    if (!strcmp(interface, "wl_compositor")) compositor = wl_registry_bind(registry, name, &wl_compositor_interface, 4);
    else if (!strcmp(interface, "wl_shm")) shm = wl_registry_bind(registry, name, &wl_shm_interface, 1);
    else if (!strcmp(interface, "xdg_wm_base")) {
        wm = wl_registry_bind(registry, name, &xdg_wm_base_interface, 1); xdg_wm_base_add_listener(wm, &wm_listener, NULL);
    } else if (!strcmp(interface, "wp_presentation")) presentation = wl_registry_bind(registry, name, &wp_presentation_interface, 1);
    else if (!strcmp(interface, "zwp_linux_dmabuf_v1") && version >= 4)
        dmabuf = wl_registry_bind(registry, name, &zwp_linux_dmabuf_v1_interface, 4);
    else if (!strcmp(interface, "wp_linux_drm_syncobj_manager_v1")) {
        REQUIRE(version == 1, "unexpected syncobj global version");
        sync_manager = wl_registry_bind(registry, name, &wp_linux_drm_syncobj_manager_v1_interface, 1);
    }
}
static void global_remove(void *data, struct wl_registry *registry, uint32_t name) { (void)data; (void)registry; (void)name; }
static const struct wl_registry_listener registry_listener = { .global = global, .global_remove = global_remove };
static void sync_output(void *data, struct wp_presentation_feedback *proxy, struct wl_output *output) { (void)data; (void)proxy; (void)output; }
static void presented(void *data, struct wp_presentation_feedback *proxy, uint32_t sh, uint32_t sl,
                      uint32_t ns, uint32_t refresh, uint32_t qh, uint32_t ql, uint32_t flags) {
    (void)refresh; struct presented *p = data; REQUIRE(ns < 1000000000, "bad presentation time");
    p->done = true; p->sequence = ((uint64_t)qh << 32) | ql;
    p->ns = (((uint64_t)sh << 32) | sl) * UINT64_C(1000000000) + ns; p->flags = flags;
    wp_presentation_feedback_destroy(proxy);
}
static void discarded(void *data, struct wp_presentation_feedback *proxy) {
    struct presented *p = data; p->done = true; p->discarded = true; wp_presentation_feedback_destroy(proxy);
}
static const struct wp_presentation_feedback_listener presentation_listener = {
    .sync_output = sync_output, .presented = presented, .discarded = discarded,
};
static void timeline_new(uint32_t *handle, struct wp_linux_drm_syncobj_timeline_v1 **proxy) {
    int fd = -1;
    REQUIRE(drmSyncobjCreate(drm_fd, 0, handle) == 0 && drmSyncobjHandleToFD(drm_fd, *handle, &fd) == 0, "create/export actual ioctl timeline");
    *proxy = wp_linux_drm_syncobj_manager_v1_import_timeline(sync_manager, fd); close(fd);
}
static bool released(struct buffer *buffer, uint64_t point) {
    uint64_t value = 0; REQUIRE(drmSyncobjQuery(drm_fd, &buffer->release, &value, 1) == 0, "query release timeline");
    return value >= point;
}
static void wait_release(struct buffer *buffer, uint64_t point) {
    uint32_t first = 0;
    REQUIRE(drmSyncobjTimelineWait(drm_fd, &buffer->release, &point, 1,
            now_ns() + (int64_t)DEADLINE_MS * 1000000, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT, &first) == 0,
            "compositor did not release actual timeline buffer");
    printf("{\"event\":\"release\",\"handle\":%u,\"point\":%" PRIu64 "}\n", buffer->release, point);
}
static bool default_offers(struct format format) {
    for (size_t i = 0; i < defaults.count; ++i)
        if (same_device(defaults.offers[i].target) && defaults.offers[i].format.fourcc == format.fourcc &&
            defaults.offers[i].format.modifier == format.modifier) return true;
    return false;
}
static struct format choose_format(struct buffer *buffer) {
    REQUIRE(same_device(defaults.main) && same_device(local.main), "feedback main device differs from named render node");
    for (size_t i = 0; i < local.count; ++i) {
        struct format f = local.offers[i].format;
        if (!same_device(local.offers[i].target) || !default_offers(f) ||
            (f.fourcc != DRM_FORMAT_ARGB8888 && f.fourcc != DRM_FORMAT_XRGB8888) || f.modifier == DRM_FORMAT_MOD_INVALID) continue;
        struct gbm_bo *bo = gbm_bo_create_with_modifiers2(gbm, SIDE, SIDE, f.fourcc, &f.modifier, 1, GBM_BO_USE_RENDERING);
        if (!bo) continue;
        REQUIRE(gbm_bo_get_modifier(bo) == f.modifier, "allocator changed advertised modifier");
        buffer->bo = bo; return f;
    }
    fail("no allocatable common advertised surface/default ARGB/XRGB modifier"); return (struct format){0};
}
static void buffer_new(struct buffer *b, struct format format) {
    if (!b->bo) b->bo = gbm_bo_create_with_modifiers2(gbm, SIDE, SIDE, format.fourcc, &format.modifier, 1, GBM_BO_USE_RENDERING);
    REQUIRE(b->bo && gbm_bo_get_modifier(b->bo) == format.modifier, "allocate actual offered DRM buffer");
    unsigned count = gbm_bo_get_plane_count(b->bo); REQUIRE(count > 0 && count <= 4, "dmabuf plane count");
    struct zwp_linux_buffer_params_v1 *params = zwp_linux_dmabuf_v1_create_params(dmabuf);
    const EGLint plane_fd[4] = { EGL_DMA_BUF_PLANE0_FD_EXT, EGL_DMA_BUF_PLANE1_FD_EXT, EGL_DMA_BUF_PLANE2_FD_EXT, EGL_DMA_BUF_PLANE3_FD_EXT };
    const EGLint plane_offset[4] = { EGL_DMA_BUF_PLANE0_OFFSET_EXT, EGL_DMA_BUF_PLANE1_OFFSET_EXT, EGL_DMA_BUF_PLANE2_OFFSET_EXT, EGL_DMA_BUF_PLANE3_OFFSET_EXT };
    const EGLint plane_pitch[4] = { EGL_DMA_BUF_PLANE0_PITCH_EXT, EGL_DMA_BUF_PLANE1_PITCH_EXT, EGL_DMA_BUF_PLANE2_PITCH_EXT, EGL_DMA_BUF_PLANE3_PITCH_EXT };
    EGLint attributes[64] = { EGL_WIDTH, SIDE, EGL_HEIGHT, SIDE, EGL_LINUX_DRM_FOURCC_EXT, (EGLint)format.fourcc };
    unsigned n = 6; int fds[4];
    for (unsigned p = 0; p < count; ++p) {
        fds[p] = gbm_bo_get_fd_for_plane(b->bo, p); REQUIRE(fds[p] >= 0, "export actual dmabuf plane fd");
        uint32_t offset = gbm_bo_get_offset(b->bo, p), stride = gbm_bo_get_stride_for_plane(b->bo, p);
        zwp_linux_buffer_params_v1_add(params, fds[p], p, offset, stride, format.modifier >> 32, format.modifier);
        attributes[n++] = plane_fd[p]; attributes[n++] = fds[p];
        attributes[n++] = plane_offset[p]; attributes[n++] = offset;
        attributes[n++] = plane_pitch[p]; attributes[n++] = stride;
        attributes[n++] = EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT + 2 * p; attributes[n++] = (uint32_t)format.modifier;
        attributes[n++] = EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT + 2 * p; attributes[n++] = format.modifier >> 32;
    }
    attributes[n++] = EGL_NONE;
    b->image = create_image(egl_display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attributes);
    for (unsigned p = 0; p < count; ++p) close(fds[p]);
    REQUIRE(b->image != EGL_NO_IMAGE_KHR, "producer EGL import of actual dmabuf");
    b->wl = zwp_linux_buffer_params_v1_create_immed(params, SIDE, SIDE, format.fourcc, 0);
    zwp_linux_buffer_params_v1_destroy(params);
    glGenRenderbuffers(1, &b->rb); glBindRenderbuffer(GL_RENDERBUFFER, b->rb);
    image_storage(GL_RENDERBUFFER, b->image);
    glGenFramebuffers(1, &b->fb); glBindFramebuffer(GL_FRAMEBUFFER, b->fb);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, b->rb);
    REQUIRE(glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE, "dmabuf render target");
    timeline_new(&b->acquire, &b->acquire_wl); timeline_new(&b->release, &b->release_wl);
}
static int paint(struct buffer *b, unsigned r, unsigned g, unsigned blue, const char *name) {
    glBindFramebuffer(GL_FRAMEBUFFER, b->fb); glViewport(0, 0, SIDE, SIDE);
    glClearColor(r / 255.f, g / 255.f, blue / 255.f, 1.f); glClear(GL_COLOR_BUFFER_BIT);
    EGLSyncKHR fence = create_sync(egl_display, EGL_SYNC_NATIVE_FENCE_ANDROID, NULL);
    REQUIRE(fence != EGL_NO_SYNC_KHR, "producer native GPU fence");
    glFlush(); int fd = dup_fence(egl_display, fence); destroy_sync(egl_display, fence);
    REQUIRE(fd >= 0, "export producer GPU sync-file");
    /* Producer readback is retained separately from independent compositor pixels. */
    unsigned char pixels[SIDE * SIDE * 4]; glReadPixels(0, 0, SIDE, SIDE, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    REQUIRE(glGetError() == GL_NO_ERROR, "producer dmabuf readback");
    for (unsigned i = 0; i < SIDE * SIDE; ++i)
        REQUIRE(abs(pixels[i * 4] - (int)r) <= 1 && abs(pixels[i * 4 + 1] - (int)g) <= 1 &&
                abs(pixels[i * 4 + 2] - (int)blue) <= 1, "producer dmabuf pixels differ");
    char path[4096]; REQUIRE(snprintf(path, sizeof(path), "%s/producer-%s.ppm", artifacts, name) < (int)sizeof(path), "artifact path length");
    FILE *out = fopen(path, "wb"); REQUIRE(out, "open readback artifact"); fprintf(out, "P6\n%d %d\n255\n", SIDE, SIDE);
    for (unsigned i = 0; i < SIDE * SIDE; ++i) REQUIRE(fwrite(pixels + i * 4, 1, 3, out) == 3, "save producer pixels");
    REQUIRE(fclose(out) == 0, "close producer readback"); return fd;
}
static void acquire_from_gpu(struct buffer *b, uint64_t point, int fd) {
    uint32_t binary = 0;
    REQUIRE(drmSyncobjCreate(drm_fd, 0, &binary) == 0 && drmSyncobjImportSyncFile(drm_fd, binary, fd) == 0 &&
            drmSyncobjTransfer(drm_fd, b->acquire, point, binary, 0, 0) == 0, "publish actual GPU acquire fence into timeline");
    close(fd); REQUIRE(drmSyncobjDestroy(drm_fd, binary) == 0, "destroy temporary import handle");
}
static void commit_buffer(struct buffer *b, uint64_t acquire, uint64_t release, struct presented *p) {
    wp_linux_drm_syncobj_surface_v1_set_acquire_point(sync_surface, b->acquire_wl, acquire >> 32, acquire);
    wp_linux_drm_syncobj_surface_v1_set_release_point(sync_surface, b->release_wl, release >> 32, release);
    if (p) wp_presentation_feedback_add_listener(wp_presentation_feedback(presentation, surface), &presentation_listener, p);
    wl_surface_attach(surface, b->wl, 0, 0); wl_surface_damage_buffer(surface, 0, 0, SIDE, SIDE); wl_surface_commit(surface);
    REQUIRE(wl_display_flush(display) >= 0, "flush actual dmabuf commit");
}
static void await_presented(struct presented *p, const char *name) {
    REQUIRE(dispatch_for(DEADLINE_MS, &p->done) && !p->discarded, "actual buffer not presented");
    printf("{\"event\":\"presented\",\"name\":\"%s\",\"sequence\":%" PRIu64 ",\"timestamp_ns\":%" PRIu64 ",\"flags\":%u}\n", name, p->sequence, p->ns, p->flags);
}
static void checkpoint(const char *name, unsigned r, unsigned g, unsigned b) {
    printf("{\"event\":\"checkpoint\",\"name\":\"%s\",\"rgb\":[%u,%u,%u]}\n", name, r, g, b); fflush(stdout);
    if (checkpoints) {
        struct pollfd p = { .fd = STDIN_FILENO, .events = POLLIN }; char line[32];
        REQUIRE(poll(&p, 1, DEADLINE_MS) == 1 && fgets(line, sizeof(line), stdin) && !strcmp(line, "continue\n"), "independent pixel capture acknowledgement");
    }
}
static void buffer_destroy(struct buffer *b) {
    if (b->wl) wl_buffer_destroy(b->wl);
    if (b->acquire_wl) wp_linux_drm_syncobj_timeline_v1_destroy(b->acquire_wl);
    if (b->release_wl) wp_linux_drm_syncobj_timeline_v1_destroy(b->release_wl);
    drmSyncobjDestroy(drm_fd, b->acquire); drmSyncobjDestroy(drm_fd, b->release);
    glDeleteFramebuffers(1, &b->fb); glDeleteRenderbuffers(1, &b->rb);
    destroy_image(egl_display, b->image); gbm_bo_destroy(b->bo);
}
static void expect_error(const char *interface, uint32_t code) {
    REQUIRE(!dispatch_for(DEADLINE_MS, NULL), "invalid request did not disconnect real client");
    const struct wl_interface *actual = NULL; uint32_t id = 0;
    uint32_t actual_code = wl_display_get_protocol_error(display, &actual, &id);
    REQUIRE(actual && !strcmp(actual->name, interface) && actual_code == code, "wrong actual protocol error interface/code");
    printf("{\"event\":\"protocol-error\",\"interface\":\"%s\",\"code\":%u,\"object_id\":%u}\n", actual->name, actual_code, id);
}
static void initialize(const char *device_path) {
    struct stat st; drm_fd = open(device_path, O_RDWR | O_CLOEXEC);
    REQUIRE(drm_fd >= 0 && fstat(drm_fd, &st) == 0 && S_ISCHR(st.st_mode), "open actual DRM render node");
    render_device = st.st_rdev; drmDevicePtr device = NULL;
    REQUIRE(drmGetDevice2(drm_fd, 0, &device) == 0 && (device->available_nodes & (1 << DRM_NODE_RENDER)), "device has no actual render node");
    struct stat render_st; REQUIRE(stat(device->nodes[DRM_NODE_RENDER], &render_st) == 0 && render_st.st_rdev == render_device, "--device must name actual render node"); drmFreeDevice(&device);
    uint64_t cap = 0; REQUIRE(drmGetCap(drm_fd, DRM_CAP_SYNCOBJ_TIMELINE, &cap) == 0 && cap, "device lacks real ioctl timeline capability");
    int ev = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK); uint32_t timeline = 0;
    REQUIRE(ev >= 0 && drmSyncobjCreate(drm_fd, 0, &timeline) == 0 && drmSyncobjEventfd(drm_fd, timeline, 1, ev, 0) == 0, "actual syncobj eventfd registration");
    drmSyncobjDestroy(drm_fd, timeline); close(ev);
    drmVersionPtr version = drmGetVersion(drm_fd); REQUIRE(version, "actual DRM driver identity");
    printf("{\"event\":\"device\",\"path\":\"%s\",\"driver\":\"%.*s\",\"timeline\":true,\"eventfd\":true}\n", device_path, version->name_len, version->name); drmFreeVersion(version);
    gbm = gbm_create_device(drm_fd); REQUIRE(gbm, "actual GBM device");
    egl_display = eglGetPlatformDisplay(EGL_PLATFORM_GBM_KHR, gbm, NULL); EGLint major, minor;
    REQUIRE(egl_display != EGL_NO_DISPLAY && eglInitialize(egl_display, &major, &minor), "initialize native GBM/EGL renderer");
    const char *extensions = eglQueryString(egl_display, EGL_EXTENSIONS);
    REQUIRE(extensions && strstr(extensions, "EGL_ANDROID_native_fence_sync") && strstr(extensions, "EGL_EXT_image_dma_buf_import_modifiers"), "native EGL fence and dmabuf modifier extensions");
    REQUIRE(eglBindAPI(EGL_OPENGL_ES_API), "bind GLES API");
    EGLint config_attributes[] = { EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT, EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_NONE };
    EGLConfig config; EGLint count;
    REQUIRE(eglChooseConfig(egl_display, config_attributes, &config, 1, &count) && count == 1, "native GLES config");
    EGLint context_attributes[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
    egl_context = eglCreateContext(egl_display, config, EGL_NO_CONTEXT, context_attributes);
    REQUIRE(egl_context != EGL_NO_CONTEXT && eglMakeCurrent(egl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, egl_context), "native GLES context");
    create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    create_sync = (PFNEGLCREATESYNCKHRPROC)eglGetProcAddress("eglCreateSyncKHR");
    destroy_sync = (PFNEGLDESTROYSYNCKHRPROC)eglGetProcAddress("eglDestroySyncKHR");
    dup_fence = (PFNEGLDUPNATIVEFENCEFDANDROIDPROC)eglGetProcAddress("eglDupNativeFenceFDANDROID");
    image_storage = (PFNGLEGLIMAGETARGETRENDERBUFFERSTORAGEOESPROC)eglGetProcAddress("glEGLImageTargetRenderbufferStorageOES");
    REQUIRE(create_image && destroy_image && create_sync && destroy_sync && dup_fence && image_storage, "native fence/image entry points");
    display = wl_display_connect(NULL); REQUIRE(display, "connect actual native compositor");
    struct wl_registry *registry = wl_display_get_registry(display); wl_registry_add_listener(registry, &registry_listener, NULL);
    REQUIRE(wl_display_roundtrip(display) >= 0 && compositor && shm && wm && dmabuf && sync_manager && presentation, "native advertised globals unavailable (no forced global permitted)");
    surface = wl_compositor_create_surface(compositor);
    sync_surface = wp_linux_drm_syncobj_manager_v1_get_surface(sync_manager, surface);
    struct zwp_linux_dmabuf_feedback_v1 *df = zwp_linux_dmabuf_v1_get_default_feedback(dmabuf);
    struct zwp_linux_dmabuf_feedback_v1 *sf = zwp_linux_dmabuf_v1_get_surface_feedback(dmabuf, surface);
    zwp_linux_dmabuf_feedback_v1_add_listener(df, &feedback_listener, &defaults);
    zwp_linux_dmabuf_feedback_v1_add_listener(sf, &feedback_listener, &local);
    REQUIRE(dispatch_for(DEADLINE_MS, &defaults.done) && dispatch_for(DEADLINE_MS, &local.done), "default/surface feedback did not complete");
    printf("{\"event\":\"feedback\",\"default_main_device\":%ju,\"surface_main_device\":%ju,\"default_formats\":%zu,\"surface_formats\":%zu}\n", (uintmax_t)defaults.main, (uintmax_t)local.main, defaults.count, local.count);
    for (size_t i = 0; i < local.count; ++i) printf("{\"event\":\"surface-tranche-format\",\"target_device\":%ju,\"flags\":%u,\"fourcc\":%u,\"modifier\":%" PRIu64 "}\n", (uintmax_t)local.offers[i].target, local.offers[i].flags, local.offers[i].format.fourcc, local.offers[i].format.modifier);
    zwp_linux_dmabuf_feedback_v1_destroy(df); zwp_linux_dmabuf_feedback_v1_destroy(sf); wl_registry_destroy(registry);
}
int main(int argc, char **argv) {
    const char *device = NULL, *scenario = "ordering";
    for (int i = 1; i < argc; ++i) {
        if (!strcmp(argv[i], "--device") && i + 1 < argc) device = argv[++i];
        else if (!strcmp(argv[i], "--artifacts") && i + 1 < argc) artifacts = argv[++i];
        else if (!strcmp(argv[i], "--case") && i + 1 < argc) scenario = argv[++i];
        else if (!strcmp(argv[i], "--checkpoints")) checkpoints = true;
        else fail("usage: --device RENDER_NODE --artifacts DIRECTORY [--case CASE] [--checkpoints]");
    }
    REQUIRE(device && artifacts, "device and artifacts are required"); setvbuf(stdout, NULL, _IOLBF, 0);
    initialize(device); struct buffer a = {0}, b = {0}; struct format format = choose_format(&a);
    printf("{\"event\":\"selected-format\",\"fourcc\":%u,\"modifier\":%" PRIu64 "}\n", format.fourcc, format.modifier);
    buffer_new(&a, format);
    if (strcmp(scenario, "ordering")) {
        const char *interface = "wp_linux_drm_syncobj_surface_v1"; uint32_t error = 0;
        if (!strcmp(scenario, "destroy") || !strcmp(scenario, "unmap") || !strcmp(scenario, "disconnect")) {
            commit_buffer(&a, 1, 2, NULL); REQUIRE(wl_display_roundtrip(display) >= 0 && !released(&a, 2), "unsignalled buffer was released before discard");
            if (!strcmp(scenario, "destroy")) { wl_surface_destroy(surface); surface = NULL; }
            else if (!strcmp(scenario, "unmap")) { wl_surface_attach(surface, NULL, 0, 0); wl_surface_commit(surface); }
            else { wl_display_disconnect(display); display = NULL; }
            if (display) REQUIRE(wl_display_roundtrip(display) >= 0, "discard request dispatch");
            wait_release(&a, 2);
            printf("{\"event\":\"case-pass\",\"case\":\"%s\"}\n", scenario); return 0;
        } else if (!strcmp(scenario, "surface-exists")) {
            wp_linux_drm_syncobj_manager_v1_get_surface(sync_manager, surface); interface = "wp_linux_drm_syncobj_manager_v1"; error = 0;
        } else if (!strcmp(scenario, "invalid-timeline")) {
            int fd = open("/dev/null", O_RDONLY | O_CLOEXEC); REQUIRE(fd >= 0, "invalid fd negative control");
            wp_linux_drm_syncobj_manager_v1_import_timeline(sync_manager, fd); close(fd); interface = "wp_linux_drm_syncobj_manager_v1"; error = 1;
        } else if (!strcmp(scenario, "no-surface")) {
            wl_surface_destroy(surface); surface = NULL; wp_linux_drm_syncobj_surface_v1_set_acquire_point(sync_surface, a.acquire_wl, 0, 1); error = 1;
        } else if (!strcmp(scenario, "no-buffer")) {
            wp_linux_drm_syncobj_surface_v1_set_release_point(sync_surface, a.release_wl, 0, 2); wl_surface_commit(surface); error = 3;
        } else if (!strcmp(scenario, "no-acquire")) {
            wp_linux_drm_syncobj_surface_v1_set_release_point(sync_surface, a.release_wl, 0, 2); wl_surface_attach(surface, a.wl, 0, 0); wl_surface_commit(surface); error = 4;
        } else if (!strcmp(scenario, "no-release")) {
            wp_linux_drm_syncobj_surface_v1_set_acquire_point(sync_surface, a.acquire_wl, 0, 1); wl_surface_attach(surface, a.wl, 0, 0); wl_surface_commit(surface); error = 5;
        } else if (!strcmp(scenario, "conflicting-points")) {
            wp_linux_drm_syncobj_surface_v1_set_acquire_point(sync_surface, a.acquire_wl, 0, 2);
            wp_linux_drm_syncobj_surface_v1_set_release_point(sync_surface, a.acquire_wl, 0, 2); wl_surface_attach(surface, a.wl, 0, 0); wl_surface_commit(surface); error = 6;
        } else if (!strcmp(scenario, "unsupported-buffer")) {
            int fd = memfd_create("actual-shm-negative-control", MFD_CLOEXEC); REQUIRE(fd >= 0 && ftruncate(fd, SIDE * SIDE * 4) == 0, "create actual shm negative control");
            struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, SIDE * SIDE * 4); close(fd);
            struct wl_buffer *unsupported = wl_shm_pool_create_buffer(pool, 0, SIDE, SIDE, SIDE * 4, WL_SHM_FORMAT_ARGB8888);
            wl_surface_attach(surface, unsupported, 0, 0); wl_surface_commit(surface); error = 2;
        } else fail("unknown case");
        REQUIRE(wl_display_flush(display) >= 0, "flush invalid request"); expect_error(interface, error);
        printf("{\"event\":\"case-pass\",\"case\":\"%s\"}\n", scenario); return 0;
    }
    buffer_new(&b, format); xdg = xdg_wm_base_get_xdg_surface(wm, surface);
    xdg_surface_add_listener(xdg, &xdg_listener, NULL); toplevel = xdg_surface_get_toplevel(xdg);
    xdg_toplevel_set_title(toplevel, "Tuna actual explicit-sync dmabuf proof");
    xdg_toplevel_set_app_id(toplevel, "org.tuna.ExplicitSyncProof");
    xdg_toplevel_set_min_size(toplevel, SIDE, SIDE); xdg_toplevel_set_max_size(toplevel, SIDE, SIDE);
    wl_surface_commit(surface); REQUIRE(dispatch_for(DEADLINE_MS, &configured), "initial xdg configure");
    acquire_from_gpu(&a, 1, paint(&a, 240, 16, 16, "red"));
    uint64_t one = 1; uint32_t first = 0;
    REQUIRE(drmSyncobjTimelineWait(drm_fd, &a.acquire, &one, 1, now_ns() + DEADLINE_MS * INT64_C(1000000), 0, &first) == 0, "already-signalled acquire prerequisite");
    struct presented red = {0}, green = {0}, blue = {0}; commit_buffer(&a, 1, 2, &red); await_presented(&red, "red");
    REQUIRE(!released(&a, 2), "renderer released a buffer still in use"); checkpoint("red-presented", 240, 16, 16);
    int green_fence = paint(&b, 16, 240, 16, "green"); commit_buffer(&b, 1, 2, &green);
    REQUIRE(wl_display_roundtrip(display) >= 0 && dispatch_for(250, NULL) && !green.done && !released(&a, 2), "acquire did not block state/presentation/release");
    checkpoint("green-blocked", 240, 16, 16);
    /* Destroy protocol timelines after commit: already committed points survive. */
    wp_linux_drm_syncobj_timeline_v1_destroy(b.acquire_wl); b.acquire_wl = NULL;
    wp_linux_drm_syncobj_timeline_v1_destroy(b.release_wl); b.release_wl = NULL;
    acquire_from_gpu(&b, 1, green_fence); await_presented(&green, "green"); wait_release(&a, 2);
    checkpoint("green-presented", 16, 240, 16);
    /* Reuse the SAME wl_buffer only after its actual release timeline is reached. */
    acquire_from_gpu(&a, 3, paint(&a, 16, 16, 240, "blue")); commit_buffer(&a, 3, 4, &blue);
    wp_linux_drm_syncobj_surface_v1_destroy(sync_surface); sync_surface = NULL;
    await_presented(&blue, "blue"); wait_release(&b, 2); checkpoint("blue-presented", 16, 16, 240);
    REQUIRE(red.ns <= green.ns && green.ns <= blue.ns && red.sequence <= green.sequence && green.sequence <= blue.sequence, "presentation ordering regressed");
    wl_surface_attach(surface, NULL, 0, 0); wl_surface_commit(surface); REQUIRE(wl_display_roundtrip(display) >= 0, "unmap used buffer"); wait_release(&a, 4);
    /* The destroyed sync-surface object may be recreated on the same surface. */
    sync_surface = wp_linux_drm_syncobj_manager_v1_get_surface(sync_manager, surface);
    REQUIRE(wl_display_roundtrip(display) >= 0, "destroy/recreate sync surface");
    buffer_destroy(&a); buffer_destroy(&b); wp_linux_drm_syncobj_surface_v1_destroy(sync_surface);
    xdg_toplevel_destroy(toplevel); xdg_surface_destroy(xdg); wl_surface_destroy(surface);
    REQUIRE(wl_display_roundtrip(display) >= 0, "client cleanup roundtrip"); wl_display_disconnect(display);
    munmap(defaults.table, defaults.table_bytes); munmap(local.table, local.table_bytes);
    eglMakeCurrent(egl_display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(egl_display, egl_context); eglTerminate(egl_display); gbm_device_destroy(gbm); close(drm_fd);
    puts("{\"event\":\"case-pass\",\"case\":\"ordering\"}"); return 0;
}
