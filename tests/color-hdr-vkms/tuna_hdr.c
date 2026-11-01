// SPDX-License-Identifier: GPL-2.0+
/* Qualification-only EMULATED hardware. No compositor admission overrides.
 * A ten-bit PQ framebuffer is consumed by the upstream VKMS u16 composer;
 * the writeback connector exposes those actual composed code values. This
 * emulates transport, not photons, panel electronics, or physical HDR fidelity.
 */
#include <linux/module.h>
#include <linux/kstrtox.h>
#include <linux/mutex.h>
#include <drm/drm_atomic.h>
#include <drm/drm_connector.h>
#include <drm/drm_atomic_helper.h>
#include <drm/drm_edid.h>
#include <drm/drm_fourcc.h>
#include <drm/drm_property.h>
#include <drm/drm_probe_helper.h>

#include "vkms_drv.h"
#include "tuna_hdr.h"
#include "tuna_edid.h"

static bool hdr_sink = true;
module_param(hdr_sink, bool, 0444);
MODULE_PARM_DESC(hdr_sink, "EMULATED HDR10 sink; false exposes an unsupported SDR sink");

/* Models the cable/sink, not compositor policy. The mutex prevents a sysfs
 * transition from calling into connector objects during device teardown.
 */
static bool connected = true;
static DEFINE_MUTEX(sink_lock);
static struct drm_device *registered_device;

static int tuna_connected_set(const char *text, const struct kernel_param *param)
{
	bool next;
	int ret;

	(void)param;
	ret = kstrtobool(text, &next);
	if (ret)
		return ret;
	mutex_lock(&sink_lock);
	if (next != READ_ONCE(connected)) {
		WRITE_ONCE(connected, next);
		if (registered_device)
			drm_connector_helper_hpd_irq_event(
				&drm_device_to_vkms_device(registered_device)->output.connector);
	}
	mutex_unlock(&sink_lock);
	return 0;
}

static const struct kernel_param_ops connected_ops = {
	.set = tuna_connected_set,
	.get = param_get_bool,
};
module_param_cb(connected, &connected_ops, &connected, 0644);
MODULE_PARM_DESC(connected, "EMULATED physical sink connection; changes generate real DRM hotplug events");

void tuna_hdr_set_registered_device(struct drm_device *dev)
{
	mutex_lock(&sink_lock);
	registered_device = dev;
	mutex_unlock(&sink_lock);
}

enum drm_connector_status tuna_hdr_detect(struct drm_connector *connector,
					 bool force)
{
	(void)connector;
	(void)force;
	return READ_ONCE(connected) ? connector_status_connected :
				     connector_status_disconnected;
}

int tuna_hdr_get_modes(struct drm_connector *connector)
{
	const struct edid *edid = (const struct edid *)(hdr_sink ?
					 tuna_edid_hdr : tuna_edid_sdr);
	int ret;
	if (!READ_ONCE(connected)) {
		drm_connector_update_edid_property(connector, NULL);
		return 0;
	}

	ret = drm_connector_update_edid_property(connector, edid);
	if (ret)
		return ret;
	return drm_add_edid_modes(connector, edid);
}

void tuna_hdr_connector_reset(struct drm_connector *connector)
{
	drm_atomic_helper_connector_reset(connector);
	if (connector->state) {
		connector->state->max_requested_bpc = hdr_sink ? 10 : 8;
		connector->state->max_bpc = hdr_sink ? 10 : 8;
	}
}

int tuna_hdr_connector_init(struct drm_connector *connector)
{
	int ret;

	/* Virtual connectors do not receive an EDID property automatically. */
	drm_connector_attach_edid_property(connector);
	tuna_hdr_connector_reset(connector);
	if (!connector->state)
		return -ENOMEM;
	ret = drm_connector_attach_max_bpc_property(connector, 8, hdr_sink ? 10 : 8);
	if (ret || !hdr_sink)
		return ret;
	ret = drm_mode_create_hdmi_colorspace_property(connector,
					 BIT(DRM_MODE_COLORIMETRY_BT2020_RGB));
	if (ret)
		return ret;
	ret = drm_connector_attach_colorspace_property(connector);
	if (ret)
		return ret;
	return drm_connector_attach_hdr_output_metadata_property(connector);
}

int tuna_hdr_atomic_check(struct drm_device *dev, struct drm_atomic_state *state)
{
	struct vkms_output *output = &drm_device_to_vkms_device(dev)->output;
	struct drm_connector_state *conn;
	struct drm_crtc_state *crtc_state;
	struct drm_plane_state *plane;
	const struct hdr_output_metadata *metadata;
	u32 format;

	/* Include unchanged connector/primary state: a plane-only commit must not
	 * silently put an 8-bit framebuffer on an already active HDR link.
	 */
	conn = drm_atomic_get_connector_state(state, &output->connector);
	if (IS_ERR(conn))
		return PTR_ERR(conn);
	if (!conn->hdr_output_metadata)
		return 0;
	if (!hdr_sink || conn->hdr_output_metadata->length != sizeof(*metadata))
		return -EINVAL;
	metadata = conn->hdr_output_metadata->data;
	if (metadata->metadata_type != 0 ||
	    metadata->hdmi_metadata_type1.metadata_type != 0 ||
	    metadata->hdmi_metadata_type1.eotf != 2 ||
	    conn->colorspace != DRM_MODE_COLORIMETRY_BT2020_RGB ||
	    conn->max_requested_bpc < 10)
		return -EINVAL;
	/* Allow a disable with retained metadata; it transports no pixels. */
	if (!conn->crtc)
		return 0;
	crtc_state = drm_atomic_get_crtc_state(state, conn->crtc);
	if (IS_ERR(crtc_state))
		return PTR_ERR(crtc_state);
	if (!crtc_state->active)
		return 0;
	if (!READ_ONCE(connected))
		return -ENODEV;
	plane = drm_atomic_get_plane_state(state, output->crtc.primary);
	if (IS_ERR(plane))
		return PTR_ERR(plane);
	if (!plane->fb || plane->crtc != conn->crtc)
		return -EINVAL;
	format = plane->fb->format->format;
	if (format != DRM_FORMAT_XRGB2101010 && format != DRM_FORMAT_ARGB2101010)
		return -EINVAL;
	return 0;
}
