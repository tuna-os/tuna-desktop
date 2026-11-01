// SPDX-License-Identifier: GPL-2.0+
/* Actual DRM ioctls only. identity is discovery, not a qualification pass.
 * selftest owns DRM master and proves XR30 frame flip + kernel composed XR30
 * writeback, all 1024 channel codes, HDR blob readback and rejected bad commits.
 * snapshot reads the compositor's active ten-bit primary GEM framebuffer and
 * HDR connector blob without taking master; run as root, save raw bytes + JSON.
 * Raw scanout memory is not a substitute for kernel writeback in selftest.
 * connection reports real GETCONNECTOR status/modes/EDID during live hotplug;
 * it does not take master or manufacture a connection result.
 */
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
#include <sys/mman.h>
#include <unistd.h>
#include <xf86drm.h>
#include <xf86drmMode.h>
#include <drm_fourcc.h>

static int fd;
static uint32_t connector, crtc, plane, writeback;
static drmModeModeInfo mode;
static bool flipped;

static void fail(const char *message)
{
	fprintf(stderr, "tuna-hdr-probe: %s: %s\n", message, strerror(errno));
	exit(1);
}

static void require(bool condition, const char *message)
{
	if (!condition) {
		errno = EINVAL;
		fail(message);
	}
}

static uint32_t property(uint32_t object, uint32_t type, const char *name,
			 uint64_t *value, const char *enum_name, uint64_t *enum_value)
{
	drmModeObjectProperties *props = drmModeObjectGetProperties(fd, object, type);
	uint32_t id = 0;
	require(props != NULL, "GETPROPERTIES");
	for (uint32_t i = 0; i < props->count_props; i++) {
		drmModePropertyRes *prop = drmModeGetProperty(fd, props->props[i]);
		require(prop != NULL, "GETPROPERTY");
		if (!strcmp(prop->name, name)) {
			id = prop->prop_id;
			if (value)
				*value = props->prop_values[i];
			if (enum_name) {
				bool found = false;
				for (int j = 0; j < prop->count_enums; j++)
					if (!strcmp(prop->enums[j].name, enum_name)) {
						*enum_value = prop->enums[j].value;
						found = true;
					}
				require(found, "missing required property enum");
			}
		}
		drmModeFreeProperty(prop);
	}
	drmModeFreeObjectProperties(props);
	return id;
}

static uint64_t value(uint32_t object, uint32_t type, const char *name)
{
	uint64_t result = 0;
	require(property(object, type, name, &result, NULL, NULL) != 0, name);
	return result;
}

static void add(drmModeAtomicReq *req, uint32_t object, uint32_t type,
		const char *name, uint64_t val)
{
	uint32_t id = property(object, type, name, NULL, NULL, NULL);
	require(id != 0, name);
	require(drmModeAtomicAddProperty(req, object, id, val) >= 0, "ATOMIC add");
}

static void discover(bool need_connected_sink)
{
	drmVersion *version = drmGetVersion(fd);
	require(version && !strcmp(version->name, "tuna_hdr_vkms"), "not our isolated emulated device");
	drmFreeVersion(version);
	require(drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1) == 0, "universal planes");
	require(drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1) == 0, "atomic client cap");
	require(drmSetClientCap(fd, DRM_CLIENT_CAP_WRITEBACK_CONNECTORS, 1) == 0, "writeback client cap");
	drmModeRes *resources = drmModeGetResources(fd);
	require(resources && resources->count_crtcs == 1, "one real emulated CRTC");
	crtc = resources->crtcs[0];
	for (int i = 0; i < resources->count_connectors; i++) {
		drmModeConnector *conn = drmModeGetConnector(fd, resources->connectors[i]);
		require(conn != NULL, "GETCONNECTOR");
		if (conn->connector_type == DRM_MODE_CONNECTOR_VIRTUAL) {
			require(!connector, "one virtual sink");
			if (need_connected_sink)
				require(conn->connection == DRM_MODE_CONNECTED && conn->count_modes, "connected virtual sink");
			connector = conn->connector_id;
			if (conn->count_modes)
				mode = conn->modes[0];
		} else if (conn->connector_type == DRM_MODE_CONNECTOR_WRITEBACK) {
			writeback = conn->connector_id;
		}
		drmModeFreeConnector(conn);
	}
	drmModeFreeResources(resources);
	require(connector && writeback, "virtual + writeback connectors");
	drmModePlaneRes *planes = drmModeGetPlaneResources(fd);
	require(planes != NULL, "GETPLANERESOURCES");
	for (uint32_t i = 0; i < planes->count_planes; i++)
		if (value(planes->planes[i], DRM_MODE_OBJECT_PLANE, "type") == DRM_PLANE_TYPE_PRIMARY)
			plane = planes->planes[i];
	drmModeFreePlaneResources(planes);
	require(plane != 0, "primary plane");
}

static struct hdr_output_metadata metadata(void)
{
	struct hdr_output_metadata result = {0};
	result.hdmi_metadata_type1.eotf = 2;
	result.hdmi_metadata_type1.display_primaries[0].x = 35400;
	result.hdmi_metadata_type1.display_primaries[0].y = 14600;
	result.hdmi_metadata_type1.display_primaries[1].x = 8500;
	result.hdmi_metadata_type1.display_primaries[1].y = 39850;
	result.hdmi_metadata_type1.display_primaries[2].x = 6550;
	result.hdmi_metadata_type1.display_primaries[2].y = 2300;
	result.hdmi_metadata_type1.white_point.x = 15635;
	result.hdmi_metadata_type1.white_point.y = 16450;
	result.hdmi_metadata_type1.max_display_mastering_luminance = 1000;
	result.hdmi_metadata_type1.min_display_mastering_luminance = 1;
	result.hdmi_metadata_type1.max_cll = 1000;
	result.hdmi_metadata_type1.max_fall = 400;
	return result;
}

struct buffer {
	uint32_t handle, fb, pitch;
	uint64_t size;
	uint8_t *data;
};

static struct buffer allocate(uint32_t format)
{
	struct drm_mode_create_dumb create = {.width = mode.hdisplay, .height = mode.vdisplay, .bpp = 32};
	struct drm_mode_map_dumb map = {0};
	struct buffer result = {0};
	require(drmIoctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &create) == 0, "CREATE_DUMB");
	result.handle = create.handle;
	result.pitch = create.pitch;
	result.size = create.size;
	uint32_t handles[4] = {create.handle}, pitches[4] = {create.pitch}, offsets[4] = {0};
	require(drmModeAddFB2(fd, mode.hdisplay, mode.vdisplay, format, handles, pitches, offsets, &result.fb, 0) == 0, "ADDFB2");
	map.handle = create.handle;
	require(drmIoctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &map) == 0, "MAP_DUMB");
	result.data = mmap(NULL, result.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, map.offset);
	require(result.data != MAP_FAILED, "mmap GEM");
	return result;
}

static uint32_t pattern(unsigned x, unsigned y, unsigned phase)
{
	uint32_t r = (x + phase) & 1023;
	uint32_t g = (x + y + 341 + phase) & 1023;
	uint32_t b = (1023 - x + y + phase) & 1023;
	return 0xc0000000u | (r << 20) | (g << 10) | b;
}

static void fill(struct buffer *buffer, unsigned phase)
{
	for (unsigned y = 0; y < mode.vdisplay; y++)
		for (unsigned x = 0; x < mode.hdisplay; x++)
			((uint32_t *)(buffer->data + y * buffer->pitch))[x] = pattern(x, y, phase);
}

static void flip_event(int event_fd, unsigned sequence, unsigned sec, unsigned usec, void *data)
{
	(void)event_fd; (void)sequence; (void)sec; (void)usec; (void)data;
	flipped = true;
}

static void wait_flip(void)
{
	drmEventContext context = {.version = DRM_EVENT_CONTEXT_VERSION, .page_flip_handler = flip_event};
	struct pollfd pollfd = {.fd = fd, .events = POLLIN};
	while (!flipped) {
		require(poll(&pollfd, 1, 15000) == 1, "real page-flip event timeout");
		require(drmHandleEvent(fd, &context) == 0, "page-flip event read");
	}
}

static void raw_write(const char *path, const uint8_t *data, uint32_t pitch)
{
	FILE *file = fopen(path, "wb");
	require(file != NULL, "open raw evidence");
	for (unsigned y = 0; y < mode.vdisplay; y++)
		require(fwrite(data + y * pitch, 4, mode.hdisplay, file) == mode.hdisplay, "write raw evidence");
	require(fclose(file) == 0, "close raw evidence");
}

static void report(const char *path, bool active, const char *pixels, const char *kind, uint32_t format)
{
	uint64_t edid_id = value(connector, DRM_MODE_OBJECT_CONNECTOR, "EDID");
	drmModePropertyBlobRes *edid = drmModeGetPropertyBlob(fd, edid_id);
	require(edid && edid->length == 256, "actual kernel EDID blob");
	FILE *file = fopen(path, "w");
	require(file != NULL, "open JSON evidence");
	fprintf(file, "{\n  \"emulated\": true, \"driver\": \"tuna_hdr_vkms\", \"operation\": \"%s\",\n", kind);
	fprintf(file, "  \"connector_id\": %u, \"crtc_id\": %u, \"primary_plane_id\": %u,\n  \"edid_hex\": \"", connector, crtc, plane);
	for (unsigned i = 0; i < edid->length; i++)
		fprintf(file, "%02x", ((uint8_t *)edid->data)[i]);
	fprintf(file, "\",\n  \"hdr_property_present\": %s",
		property(connector, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA", NULL, NULL, NULL) ? "true" : "false");
	drmModeFreePropertyBlob(edid);
	if (active) {
		uint64_t hdr = value(connector, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");
		uint64_t colorspace = 0;
		require(property(connector, DRM_MODE_OBJECT_CONNECTOR, "Colorspace", NULL, "BT2020_RGB", &colorspace) != 0, "BT2020 enum");
		require(value(connector, DRM_MODE_OBJECT_CONNECTOR, "Colorspace") == colorspace, "active BT2020 output");
		require(value(connector, DRM_MODE_OBJECT_CONNECTOR, "max bpc") == 10, "active ten-bit link");
		require(value(connector, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID") == crtc, "active connector");
		require(value(crtc, DRM_MODE_OBJECT_CRTC, "ACTIVE") == 1, "active CRTC");
		drmModePropertyBlobRes *blob = drmModeGetPropertyBlob(fd, hdr);
		require(blob && blob->length == sizeof(struct hdr_output_metadata), "actual HDR metadata blob");
		struct hdr_output_metadata *m = blob->data;
		require(m->metadata_type == 0 && m->hdmi_metadata_type1.eotf == 2 && m->hdmi_metadata_type1.metadata_type == 0, "active PQ/type1 metadata");
		fprintf(file, ",\n  \"active_hdr_metadata_hex\": \"");
		for (unsigned i = 0; i < blob->length; i++)
			fprintf(file, "%02x", ((uint8_t *)blob->data)[i]);
		fprintf(file, "\", \"colorspace\": \"BT2020_RGB\", \"max_bpc\": 10,\n"
			"  \"width\": %u, \"height\": %u, \"stride\": %u, \"fourcc\": %u, \"format\": \"%s\", "
			"\"raw_layout\": \"little-endian-u32-B10-G10-R10-%s2-tight-rows\", \"pixels\": \"%s\"",
			mode.hdisplay, mode.vdisplay, mode.hdisplay * 4u, format,
			format == DRM_FORMAT_ARGB2101010 ? "AR30" : "XR30",
			format == DRM_FORMAT_ARGB2101010 ? "A" : "X", pixels);
		drmModeFreePropertyBlob(blob);
	}
	if (!strcmp(kind, "selftest"))
		fprintf(file, ",\n  \"page_flip_events\": 1, \"writeback_fence_signaled\": true, \"all_composed_pixels_match\": true, \"eight_bit_hdr_rejected\": true, \"non_pq_metadata_rejected\": true");
	fprintf(file, "\n}\n");
	require(fclose(file) == 0, "close JSON evidence");
}

static void rejected(drmModeAtomicReq *request, const char *message)
{
	errno = 0;
	int result = drmModeAtomicCommit(fd, request, DRM_MODE_ATOMIC_TEST_ONLY | DRM_MODE_ATOMIC_ALLOW_MODESET, NULL);
	require(result < 0 && errno == EINVAL, message);
	drmModeAtomicFree(request);
}

static void selftest(const char *json, const char *raw)
{
	require(drmSetMaster(fd) == 0, "DRM master (stop only your owned compositor first)");
	require(mode.hdisplay >= 1024, "mode covers all ten-bit code values");
	struct buffer first = allocate(DRM_FORMAT_XRGB2101010);
	struct buffer second = allocate(DRM_FORMAT_XRGB2101010);
	struct buffer eight = allocate(DRM_FORMAT_XRGB8888);
	struct buffer wb = allocate(DRM_FORMAT_XRGB2101010);
	fill(&first, 0);
	fill(&second, 1);
	uint32_t mode_blob, hdr_blob, bad_blob;
	struct hdr_output_metadata hdr = metadata();
	require(drmModeCreatePropertyBlob(fd, &mode, sizeof(mode), &mode_blob) == 0, "mode blob");
	require(drmModeCreatePropertyBlob(fd, &hdr, sizeof(hdr), &hdr_blob) == 0, "HDR blob");
	uint64_t bt2020 = 0;
	require(property(connector, DRM_MODE_OBJECT_CONNECTOR, "Colorspace", NULL, "BT2020_RGB", &bt2020) != 0, "HDR sink required");
	drmModeAtomicReq *req = drmModeAtomicAlloc();
	require(req != NULL, "atomic allocation");
	add(req, connector, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", crtc);
	add(req, connector, DRM_MODE_OBJECT_CONNECTOR, "Colorspace", bt2020);
	add(req, connector, DRM_MODE_OBJECT_CONNECTOR, "max bpc", 10);
	add(req, connector, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA", hdr_blob);
	add(req, crtc, DRM_MODE_OBJECT_CRTC, "ACTIVE", 1);
	add(req, crtc, DRM_MODE_OBJECT_CRTC, "MODE_ID", mode_blob);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "CRTC_ID", crtc);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "FB_ID", first.fb);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "SRC_X", 0);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "SRC_Y", 0);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "SRC_W", (uint64_t)mode.hdisplay << 16);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "SRC_H", (uint64_t)mode.vdisplay << 16);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "CRTC_X", 0);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "CRTC_Y", 0);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "CRTC_W", mode.hdisplay);
	add(req, plane, DRM_MODE_OBJECT_PLANE, "CRTC_H", mode.vdisplay);
	require(drmModeAtomicCommit(fd, req, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL) == 0, "real HDR modeset");
	drmModeAtomicFree(req);
	req = drmModeAtomicAlloc();
	require(req != NULL, "atomic allocation");
	add(req, plane, DRM_MODE_OBJECT_PLANE, "FB_ID", second.fb);
	require(drmModeAtomicCommit(fd, req, DRM_MODE_PAGE_FLIP_EVENT | DRM_MODE_ATOMIC_NONBLOCK, NULL) == 0, "real XR30 page flip");
	drmModeAtomicFree(req);
	wait_flip();
	int fence = -1;
	req = drmModeAtomicAlloc();
	require(req != NULL, "atomic allocation");
	add(req, writeback, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", crtc);
	add(req, writeback, DRM_MODE_OBJECT_CONNECTOR, "WRITEBACK_FB_ID", wb.fb);
	add(req, writeback, DRM_MODE_OBJECT_CONNECTOR, "WRITEBACK_OUT_FENCE_PTR", (uintptr_t)&fence);
	require(drmModeAtomicCommit(fd, req, 0, NULL) == 0, "real composed writeback commit");
	drmModeAtomicFree(req);
	require(fence >= 0, "real writeback fence fd");
	struct pollfd completion = {.fd = fence, .events = POLLIN};
	require(poll(&completion, 1, 15000) == 1 && (completion.revents & POLLIN), "writeback completion fence");
	close(fence);
	for (unsigned y = 0; y < mode.vdisplay; y++)
		for (unsigned x = 0; x < mode.hdisplay; x++)
			require(((uint32_t *)(wb.data + y * wb.pitch))[x] == pattern(x, y, 1), "actual composed ten-bit pixel mismatch");
	uint64_t read_id = value(connector, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA");
	drmModePropertyBlobRes *readback = drmModeGetPropertyBlob(fd, read_id);
	require(readback && readback->length == sizeof(hdr) && !memcmp(readback->data, &hdr, sizeof(hdr)), "exact active metadata readback");
	drmModeFreePropertyBlob(readback);
	req = drmModeAtomicAlloc();
	require(req != NULL, "atomic allocation");
	add(req, plane, DRM_MODE_OBJECT_PLANE, "FB_ID", eight.fb);
	rejected(req, "8-bit HDR commit was not rejected by kernel");
	hdr.hdmi_metadata_type1.eotf = 0;
	require(drmModeCreatePropertyBlob(fd, &hdr, sizeof(hdr), &bad_blob) == 0, "bad metadata blob");
	req = drmModeAtomicAlloc();
	require(req != NULL, "atomic allocation");
	add(req, connector, DRM_MODE_OBJECT_CONNECTOR, "HDR_OUTPUT_METADATA", bad_blob);
	rejected(req, "non-PQ HDR metadata was not rejected by kernel");
	raw_write(raw, wb.data, wb.pitch);
	report(json, true, "kernel-composed-writeback", "selftest", DRM_FORMAT_XRGB2101010);
	/* File close releases all GEM handles and blob references. The isolated
	 * run wrapper then shuts down and unloads precisely this device.
	 */
}

static void snapshot(const char *json, const char *raw)
{
	uint32_t id = value(plane, DRM_MODE_OBJECT_PLANE, "FB_ID");
	drmModeFB2 *fb = drmModeGetFB2(fd, id);
	require(fb && fb->handles[0] &&
		(fb->pixel_format == DRM_FORMAT_XRGB2101010 ||
		 fb->pixel_format == DRM_FORMAT_ARGB2101010),
		"actual root-readable active RGB10 framebuffer");
	require(fb->modifier == DRM_FORMAT_MOD_LINEAR || fb->modifier == DRM_FORMAT_MOD_INVALID, "linear framebuffer");
	drmModeCrtc *active = drmModeGetCrtc(fd, crtc);
	require(active && active->mode_valid, "actual active CRTC mode");
	mode = active->mode;
	drmModeFreeCrtc(active);
	require(fb->width == mode.hdisplay && fb->height == mode.vdisplay, "unscaled full-output primary");
	struct drm_mode_map_dumb map = {.handle = fb->handles[0]};
	require(drmIoctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &map) == 0, "active framebuffer GEM map");
	size_t length = (size_t)fb->pitches[0] * fb->height + fb->offsets[0];
	uint8_t *pixels = mmap(NULL, length, PROT_READ, MAP_SHARED, fd, map.offset);
	require(pixels != MAP_FAILED, "active framebuffer mmap");
	raw_write(raw, pixels + fb->offsets[0], fb->pitches[0]);
	report(json, true, "active-primary-GEM-not-writeback", "snapshot", fb->pixel_format);
	munmap(pixels, length);
	drmModeFreeFB2(fb);
}

static void connection_report(const char *path)
{
	drmModeConnector *conn = drmModeGetConnector(fd, connector);
	require(conn != NULL, "live GETCONNECTOR");
	const char *status = conn->connection == DRM_MODE_CONNECTED ? "connected" :
		conn->connection == DRM_MODE_DISCONNECTED ? "disconnected" : "unknown";
	uint64_t edid_id = value(connector, DRM_MODE_OBJECT_CONNECTOR, "EDID");
	drmModePropertyBlobRes *edid = edid_id ? drmModeGetPropertyBlob(fd, edid_id) : NULL;
	require(!edid_id || edid, "live EDID blob readback");
	FILE *file = fopen(path, "w");
	require(file != NULL, "open live connection receipt");
	fprintf(file, "{\n  \"emulated\": true, \"driver\": \"tuna_hdr_vkms\", \"operation\": \"connection\",\n  \"connector_id\": %u, \"connection\": \"%s\", \"connected\": %s, \"mode_count\": %d,\n  \"crtc_id\": %" PRIu64 ", \"edid_present\": %s, \"edid_hex\": \"",
		connector, status, conn->connection == DRM_MODE_CONNECTED ? "true" : "false",
		conn->count_modes, value(connector, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID"),
		edid ? "true" : "false");
	if (edid)
		for (unsigned i = 0; i < edid->length; i++)
			fprintf(file, "%02x", ((uint8_t *)edid->data)[i]);
	fprintf(file, "\"\n}\n");
	require(fclose(file) == 0, "close live connection receipt");
	if (edid)
		drmModeFreePropertyBlob(edid);
	drmModeFreeConnector(conn);
}

int main(int argc, char **argv)
{
	require(argc == 4 || argc == 5, "usage: identity|connection CARD JSON | selftest|snapshot CARD JSON RAW");
	fd = open(argv[2], O_RDWR | O_CLOEXEC);
	if (fd < 0)
		fail("open isolated DRM card");
	discover(strcmp(argv[1], "connection") != 0);
	if (!strcmp(argv[1], "connection") && argc == 4)
		connection_report(argv[3]);
	else if (!strcmp(argv[1], "identity") && argc == 4)
		report(argv[3], false, NULL, "identity", 0);
	else if (!strcmp(argv[1], "selftest") && argc == 5)
		selftest(argv[3], argv[4]);
	else if (!strcmp(argv[1], "snapshot") && argc == 5)
		snapshot(argv[3], argv[4]);
	else
		require(false, "invalid operation/arguments");
	close(fd);
	return 0;
}
