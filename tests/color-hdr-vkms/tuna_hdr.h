/* SPDX-License-Identifier: GPL-2.0+ */
#ifndef TUNA_HDR_H
#define TUNA_HDR_H
#include <drm/drm_connector.h>

struct drm_connector;
struct drm_device;
struct drm_atomic_state;
void tuna_hdr_connector_reset(struct drm_connector *connector);
enum drm_connector_status tuna_hdr_detect(struct drm_connector *connector,
					 bool force);
void tuna_hdr_set_registered_device(struct drm_device *dev);
int tuna_hdr_get_modes(struct drm_connector *connector);
int tuna_hdr_connector_init(struct drm_connector *connector);
int tuna_hdr_atomic_check(struct drm_device *dev, struct drm_atomic_state *state);

#endif
