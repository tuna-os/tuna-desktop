// SPDX-License-Identifier: GPL-3.0-or-later
/* Independent GBM/EGL/GLES2 DMA-heap R8 sampling oracle, not release admission.
 * Build: cc -std=c11 -Wall -Wextra -Werror -O0 -g r8-import.c -o r8-import \
 *            $(pkg-config --cflags --libs gbm egl glesv2 libdrm)
 * Run: ./r8-import --device /dev/dri/renderD128 --dma-heap /dev/dma_heap/system
 * Dependencies: a C compiler, pkg-config, GBM/EGL/GLES2/libdrm development files,
 * and Linux UAPI headers providing linux/dma-heap.h and linux/dma-buf.h.
 * There is deliberately no software override, alternate context, or skip path.
 * A parent may bound execution and disable cores externally; no signal handler
 * obscures a fault in the driver. All live objects survive both pixel oracles.
 */
#define _GNU_SOURCE
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <drm_fourcc.h>
#include <errno.h>
#include <fcntl.h>
#include <gbm.h>
#include <inttypes.h>
#include <linux/dma-buf.h>
#include <linux/dma-heap.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>
#include <xf86drm.h>

enum { SIDE = 8, PIXELS = SIDE * SIDE, MAX_CAPS = 1024 };
static struct {
	int device_fd, heap_fd, dma_fd;
	struct gbm_device *gbm;
	EGLDisplay display;
	EGLContext context;
	EGLImageKHR image;
	bool initialized, current, cpu_sync;
	void *mapping;
	size_t allocation;
	GLuint vertex, fragment, program, vbo, fbo;
	GLuint rgba_texture, r8_texture, target_texture;
	PFNEGLDESTROYIMAGEKHRPROC destroy_image;
} state = {
	.device_fd = -1, .heap_fd = -1, .dma_fd = -1,
	.display = EGL_NO_DISPLAY, .context = EGL_NO_CONTEXT,
	.image = EGL_NO_IMAGE_KHR, .mapping = MAP_FAILED,
};

static bool cleanup(void);

static void stage(const char *name)
{
	printf("R8-REPRO stage=%s\n", name);
}

static void fail(const char *message)
{
	fprintf(stderr, "R8-REPRO FAIL %s\n", message);
	(void)cleanup();
	exit(EXIT_FAILURE);
}

static void require(bool condition, const char *message)
{
	if (!condition)
		fail(message);
}

static void system_fail(const char *operation)
{
	fprintf(stderr, "R8-REPRO syscall=%s errno=%d (%s)\n",
		operation, errno, strerror(errno));
	fail(operation);
}

static void gl_ok(const char *operation)
{
	GLenum error = glGetError();
	if (error != GL_NO_ERROR) {
		fprintf(stderr, "R8-REPRO GL operation=%s error=0x%04x\n", operation, error);
		fail("GLES API error");
	}
}

#define GL(call) do { call; gl_ok(#call); } while (0)

static void egl_ok(const char *operation)
{
	EGLint error = eglGetError();
	if (error != EGL_SUCCESS) {
		fprintf(stderr, "R8-REPRO EGL operation=%s error=0x%04x\n", operation, error);
		fail("EGL API error");
	}
}

#define EGL_BOOL(call) do { \
	EGLBoolean result_ = (call); \
	egl_ok(#call); \
	require(result_ == EGL_TRUE, #call); \
} while (0)

static bool extension(const char *extensions, const char *name)
{
	size_t length = strlen(name);
	const char *found = extensions;
	while ((found = strstr(found, name)) != NULL) {
		if ((found == extensions || found[-1] == ' ') &&
		    (found[length] == '\0' || found[length] == ' '))
			return true;
		found += length;
	}
	return false;
}

/* A driver cannot flood the receipt with an unbounded extension/log string. */
static void identity(const char *name, const char *value)
{
	require(value != NULL, name);
	printf("R8-REPRO %s=%.*s%s\n", name, 32768, value,
		strlen(value) > 32768 ? " [truncated]" : "");
}

static const char *egl_string(EGLDisplay display, EGLint token, const char *name)
{
	const char *value = eglQueryString(display, token);
	egl_ok(name);
	identity(name, value);
	return value;
}

static const char *gl_string(GLenum token, const char *name)
{
	const char *value = (const char *)glGetString(token);
	gl_ok(name);
	identity(name, value);
	return value;
}

static void check_fd(int fd, const char *name, bool character)
{
	struct stat info;
	if (fstat(fd, &info) != 0)
		system_fail("fstat");
	printf("R8-REPRO fd=%s size=%jd mode=%jo device=%u:%u\n", name,
		(intmax_t)info.st_size, (uintmax_t)info.st_mode,
		major(info.st_rdev), minor(info.st_rdev));
	if (character)
		require(S_ISCHR(info.st_mode), "device/heap must be a character device");
}

static void create_context(const char *device)
{
	stage("device-open.begin");
	state.device_fd = open(device, O_RDWR | O_CLOEXEC);
	if (state.device_fd < 0)
		system_fail("open DRM device");
	check_fd(state.device_fd, "drm", true);
	drmVersionPtr version = drmGetVersion(state.device_fd);
	if (!version)
		system_fail("drmGetVersion");
	printf("R8-REPRO DRM version=%d.%d.%d name=%.*s description=%.*s\n",
		version->version_major, version->version_minor, version->version_patchlevel,
		version->name_len < 4096 ? version->name_len : 4096,
		version->name ? version->name : "",
		version->desc_len < 4096 ? version->desc_len : 4096,
		version->desc ? version->desc : "");
	drmFreeVersion(version);
	stage("gbm-create.begin");
	state.gbm = gbm_create_device(state.device_fd);
	require(state.gbm != NULL, "gbm_create_device");
	identity("GBM.backend", gbm_device_get_backend_name(state.gbm));
	require(gbm_device_get_fd(state.gbm) == state.device_fd, "GBM device fd changed");

	const char *client_extensions = egl_string(EGL_NO_DISPLAY, EGL_EXTENSIONS,
		"EGL.client_extensions");
	require(extension(client_extensions, "EGL_EXT_platform_base"),
		"required EGL_EXT_platform_base unsupported");
	require(extension(client_extensions, "EGL_KHR_platform_gbm") ||
		extension(client_extensions, "EGL_MESA_platform_gbm"),
		"required GBM EGL platform unsupported");
	PFNEGLGETPLATFORMDISPLAYEXTPROC get_display =
		(PFNEGLGETPLATFORMDISPLAYEXTPROC)eglGetProcAddress("eglGetPlatformDisplayEXT");
	egl_ok("eglGetProcAddress eglGetPlatformDisplayEXT");
	require(get_display != NULL, "eglGetPlatformDisplayEXT unavailable");
	stage("egl-initialize.begin");
	state.display = get_display(EGL_PLATFORM_GBM_KHR, state.gbm, NULL);
	egl_ok("eglGetPlatformDisplayEXT");
	require(state.display != EGL_NO_DISPLAY, "GBM EGL display unavailable");
	EGLint major_version, minor_version;
	EGL_BOOL(eglInitialize(state.display, &major_version, &minor_version));
	state.initialized = true;
	printf("R8-REPRO EGL initialized=%d.%d\n", major_version, minor_version);
	egl_string(state.display, EGL_VENDOR, "EGL.vendor");
	egl_string(state.display, EGL_VERSION, "EGL.version");
	egl_string(state.display, EGL_CLIENT_APIS, "EGL.client_apis");
	const char *extensions = egl_string(state.display, EGL_EXTENSIONS, "EGL.extensions");
	const char *required[] = {
		"EGL_KHR_no_config_context", "EGL_KHR_surfaceless_context",
		"EGL_KHR_image_base", "EGL_EXT_image_dma_buf_import",
		"EGL_EXT_image_dma_buf_import_modifiers",
	};
	for (size_t i = 0; i < sizeof(required) / sizeof(required[0]); i++)
		require(extension(extensions, required[i]), required[i]);
	EGL_BOOL(eglBindAPI(EGL_OPENGL_ES_API));
	/* Exactly the old default's ES2 request and GLSL 100, not a desktop GL or
	 * ES3-only program. A driver may return a compatible newer ES context. */
	const EGLint attributes[] = { EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE };
	stage("egl-context-create.begin");
	state.context = eglCreateContext(state.display, EGL_NO_CONFIG_KHR,
		EGL_NO_CONTEXT, attributes);
	egl_ok("eglCreateContext");
	require(state.context != EGL_NO_CONTEXT, "ES2 context unavailable");
	EGL_BOOL(eglMakeCurrent(state.display, EGL_NO_SURFACE, EGL_NO_SURFACE, state.context));
	state.current = true;
	EGLint actual_version, actual_type;
	EGL_BOOL(eglQueryContext(state.display, state.context, EGL_CONTEXT_CLIENT_VERSION, &actual_version));
	EGL_BOOL(eglQueryContext(state.display, state.context, EGL_CONTEXT_CLIENT_TYPE, &actual_type));
	printf("R8-REPRO EGL context_client_version=%d context_client_type=0x%x\n",
		actual_version, actual_type);
	require(actual_version >= 2 && actual_type == EGL_OPENGL_ES_API, "unexpected context API");
	gl_string(GL_VENDOR, "GL.vendor");
	gl_string(GL_RENDERER, "GL.renderer");
	gl_string(GL_VERSION, "GL.version");
	gl_string(GL_SHADING_LANGUAGE_VERSION, "GL.shading_language_version");
	const char *gl_extensions = gl_string(GL_EXTENSIONS, "GL.extensions");
	require(extension(gl_extensions, "GL_OES_EGL_image"), "required GL_OES_EGL_image unsupported");
	require(extension(gl_extensions, "GL_EXT_texture_rg"), "required GL_EXT_texture_rg unsupported");
}

static void import_capabilities(void)
{
	stage("r8-capabilities.begin");
	PFNEGLQUERYDMABUFFORMATSEXTPROC query_formats =
		(PFNEGLQUERYDMABUFFORMATSEXTPROC)eglGetProcAddress("eglQueryDmaBufFormatsEXT");
	egl_ok("eglGetProcAddress eglQueryDmaBufFormatsEXT");
	PFNEGLQUERYDMABUFMODIFIERSEXTPROC query_modifiers =
		(PFNEGLQUERYDMABUFMODIFIERSEXTPROC)eglGetProcAddress("eglQueryDmaBufModifiersEXT");
	egl_ok("eglGetProcAddress eglQueryDmaBufModifiersEXT");
	require(query_formats && query_modifiers, "DMA-BUF capability entrypoints unavailable");
	EGLint count = 0, formats[MAX_CAPS];
	EGL_BOOL(query_formats(state.display, 0, NULL, &count));
	printf("R8-REPRO EGL dma_buf_format_count=%d\n", count);
	require(count > 0 && count <= MAX_CAPS, "invalid/excessive DMA-BUF format count");
	EGLint returned = 0;
	EGL_BOOL(query_formats(state.display, count, formats, &returned));
	require(returned == count, "DMA-BUF format count changed");
	bool r8 = false;
	for (EGLint i = 0; i < count; i++)
		if (formats[i] == (EGLint)DRM_FORMAT_R8)
			r8 = true;
	printf("R8-REPRO EGL R8_advertised=%d\n", r8);
	require(r8, "R8 DMA-BUF format unsupported");
	EGLuint64KHR modifiers[MAX_CAPS];
	EGLBoolean external_only[MAX_CAPS];
	EGL_BOOL(query_modifiers(state.display, DRM_FORMAT_R8, 0, NULL, NULL, &count));
	printf("R8-REPRO EGL R8_modifier_count=%d\n", count);
	require(count > 0 && count <= MAX_CAPS, "invalid/excessive R8 modifier count");
	EGL_BOOL(query_modifiers(state.display, DRM_FORMAT_R8, count, modifiers, external_only, &returned));
	require(returned == count, "R8 modifier count changed");
	bool linear_2d = false;
	for (EGLint i = 0; i < count; i++) {
		printf("R8-REPRO EGL R8_modifier=0x%016" PRIx64 " external_only=%u\n",
			(uint64_t)modifiers[i], external_only[i]);
		if (modifiers[i] == DRM_FORMAT_MOD_LINEAR && external_only[i] == EGL_FALSE)
			linear_2d = true;
	}
	require(linear_2d, "linear R8 GL_TEXTURE_2D import unsupported");
	stage("r8-capabilities.end");
}

static void shader_log(GLuint object, bool program)
{
	char log[4096];
	GLsizei length = 0;
	if (program)
		GL(glGetProgramInfoLog(object, sizeof(log), &length, log));
	else
		GL(glGetShaderInfoLog(object, sizeof(log), &length, log));
	printf("R8-REPRO %s_log=%.*s\n", program ? "program" : "shader", (int)length, log);
}

static void compile(GLuint *object, GLenum kind, const char *source)
{
	*object = glCreateShader(kind);
	gl_ok("glCreateShader");
	require(*object != 0, "glCreateShader returned zero");
	GL(glShaderSource(*object, 1, &source, NULL));
	GL(glCompileShader(*object));
	GLint success;
	GL(glGetShaderiv(*object, GL_COMPILE_STATUS, &success));
	shader_log(*object, false);
	require(success == GL_TRUE, "shader compile failed");
}

static void texture_parameters(void)
{
	GL(glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST));
	GL(glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST));
	GL(glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE));
	GL(glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE));
}

static void create_pipeline(uint8_t rgba[PIXELS][4])
{
	static const char vertex[] =
		"#version 100\n"
		"attribute vec2 position; attribute vec2 uv; varying mediump vec2 v_uv;\n"
		"void main() { gl_Position = vec4(position, 0.0, 1.0); v_uv = uv; }\n";
	static const char fragment[] =
		"#version 100\nprecision mediump float;\n"
		"varying mediump vec2 v_uv; uniform sampler2D tex;\n"
		"void main() { gl_FragColor = texture2D(tex, v_uv); }\n";
	/* Pixel (x,y) is sampled at ((x+.5)/8,(y+.5)/8). Neither oracle has
	 * constant input; flipped rows, stale data and wrong strides fail. */
	static const GLfloat vertices[][4] = {
		{ -1, -1, 0, 0 }, { 1, -1, 1, 0 },
		{ -1, 1, 0, 1 }, { 1, 1, 1, 1 },
	};
	stage("pipeline-create.begin");
	compile(&state.vertex, GL_VERTEX_SHADER, vertex);
	compile(&state.fragment, GL_FRAGMENT_SHADER, fragment);
	state.program = glCreateProgram();
	gl_ok("glCreateProgram");
	require(state.program != 0, "glCreateProgram returned zero");
	GL(glAttachShader(state.program, state.vertex));
	GL(glAttachShader(state.program, state.fragment));
	GL(glBindAttribLocation(state.program, 0, "position"));
	GL(glBindAttribLocation(state.program, 1, "uv"));
	GL(glLinkProgram(state.program));
	GLint success;
	GL(glGetProgramiv(state.program, GL_LINK_STATUS, &success));
	shader_log(state.program, true);
	require(success == GL_TRUE, "program link failed");
	GL(glUseProgram(state.program));
	GLint sampler = glGetUniformLocation(state.program, "tex");
	gl_ok("glGetUniformLocation");
	require(sampler >= 0, "sampler uniform missing");
	GL(glUniform1i(sampler, 0));
	GL(glGenBuffers(1, &state.vbo));
	require(state.vbo != 0, "VBO allocation failed");
	GL(glBindBuffer(GL_ARRAY_BUFFER, state.vbo));
	GL(glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW));
	GL(glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 4 * sizeof(GLfloat), (const void *)(uintptr_t)0));
	GL(glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 4 * sizeof(GLfloat), (const void *)(uintptr_t)(2 * sizeof(GLfloat))));
	GL(glEnableVertexAttribArray(0));
	GL(glEnableVertexAttribArray(1));
	GL(glActiveTexture(GL_TEXTURE0));
	GL(glPixelStorei(GL_UNPACK_ALIGNMENT, 1));
	GL(glPixelStorei(GL_PACK_ALIGNMENT, 1));
	GL(glGenTextures(1, &state.rgba_texture));
	require(state.rgba_texture != 0, "RGBA texture allocation failed");
	GL(glBindTexture(GL_TEXTURE_2D, state.rgba_texture));
	texture_parameters();
	GL(glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, SIDE, SIDE, 0, GL_RGBA, GL_UNSIGNED_BYTE, rgba));
	GL(glGenTextures(1, &state.target_texture));
	require(state.target_texture != 0, "target texture allocation failed");
	GL(glBindTexture(GL_TEXTURE_2D, state.target_texture));
	texture_parameters();
	GL(glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, SIDE, SIDE, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL));
	GL(glGenFramebuffers(1, &state.fbo));
	require(state.fbo != 0, "FBO allocation failed");
	GL(glBindFramebuffer(GL_FRAMEBUFFER, state.fbo));
	GL(glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, state.target_texture, 0));
	GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
	gl_ok("glCheckFramebufferStatus");
	printf("R8-REPRO GL framebuffer_status=0x%04x\n", status);
	require(status == GL_FRAMEBUFFER_COMPLETE, "incomplete framebuffer");
	const GLenum bits[] = { GL_RED_BITS, GL_GREEN_BITS, GL_BLUE_BITS, GL_ALPHA_BITS };
	for (size_t i = 0; i < sizeof(bits) / sizeof(bits[0]); i++) {
		GLint value;
		GL(glGetIntegerv(bits[i], &value));
		printf("R8-REPRO GL framebuffer_channel=%zu bits=%d\n", i, value);
		require(value == 8, "oracle requires RGBA8 framebuffer");
	}
	GL(glViewport(0, 0, SIDE, SIDE));
	GL(glDisable(GL_BLEND));
	GL(glDisable(GL_DITHER));
	GL(glDisable(GL_SCISSOR_TEST));
	GL(glDisable(GL_DEPTH_TEST));
	GL(glDisable(GL_STENCIL_TEST));
	GL(glDisable(GL_CULL_FACE));
	GL(glColorMask(GL_TRUE, GL_TRUE, GL_TRUE, GL_TRUE));
	stage("pipeline-create.end");
}

static void draw_and_compare(GLuint texture, const uint8_t expected[PIXELS][4], const char *name)
{
	printf("R8-REPRO case=%s begin\n", name);
	GL(glBindTexture(GL_TEXTURE_2D, texture));
	GL(glValidateProgram(state.program));
	GLint valid;
	GL(glGetProgramiv(state.program, GL_VALIDATE_STATUS, &valid));
	shader_log(state.program, true);
	require(valid == GL_TRUE, "program validation failed");
	/* The sentinel cannot equal any expected pixel because alpha is zero. */
	GL(glClearColor(1.0f, 0.0f, 1.0f, 0.0f));
	GL(glClear(GL_COLOR_BUFFER_BIT));
	stage("draw-arrays.begin");
	GL(glDrawArrays(GL_TRIANGLE_STRIP, 0, 4));
	stage("draw-arrays.end");
	stage("finish.begin");
	GL(glFinish());
	stage("finish.end");
	uint8_t actual[PIXELS][4];
	memset(actual, 0xa5, sizeof(actual));
	stage("read-pixels.begin");
	GL(glReadPixels(0, 0, SIDE, SIDE, GL_RGBA, GL_UNSIGNED_BYTE, actual));
	stage("read-pixels.end");
	unsigned mismatches = 0;
	for (unsigned i = 0; i < PIXELS; i++) {
		if (memcmp(actual[i], expected[i], 4) != 0) {
			if (mismatches < 8)
				fprintf(stderr, "R8-REPRO case=%s pixel=(%u,%u) actual=%u,%u,%u,%u expected=%u,%u,%u,%u\n",
					name, i % SIDE, i / SIDE,
					actual[i][0], actual[i][1], actual[i][2], actual[i][3],
					expected[i][0], expected[i][1], expected[i][2], expected[i][3]);
			mismatches++;
		}
	}
	printf("R8-REPRO case=%s checked_pixels=%d mismatches=%u\n", name, PIXELS, mismatches);
	require(mismatches == 0, "exact pixel oracle failed");
	printf("R8-REPRO case=%s PASS\n", name);
}

static void dma_sync(uint64_t flags)
{
	struct dma_buf_sync sync = { .flags = flags };
	int result;
	do {
		result = ioctl(state.dma_fd, DMA_BUF_IOCTL_SYNC, &sync);
	} while (result < 0 && (errno == EINTR || errno == EAGAIN));
	if (result != 0)
		system_fail("DMA_BUF_IOCTL_SYNC");
}

static void create_r8(const char *heap, const uint8_t expected[PIXELS][4])
{
	stage("dma-heap-open.begin");
	state.heap_fd = open(heap, O_RDWR | O_CLOEXEC);
	if (state.heap_fd < 0)
		system_fail("open DMA heap");
	check_fd(state.heap_fd, "dma-heap", true);
	long page = sysconf(_SC_PAGESIZE);
	require(page > 0 && (uintmax_t)page <= SIZE_MAX - PIXELS, "invalid page size");
	state.allocation = ((PIXELS + (size_t)page - 1) / (size_t)page) * (size_t)page;
	struct dma_heap_allocation_data allocation = {
		.len = state.allocation, .fd_flags = O_RDWR | O_CLOEXEC, .heap_flags = 0,
	};
	stage("dma-heap-allocate.begin");
	if (ioctl(state.heap_fd, DMA_HEAP_IOCTL_ALLOC, &allocation) != 0)
		system_fail("DMA_HEAP_IOCTL_ALLOC");
	require(allocation.fd <= INT32_MAX, "DMA heap returned out-of-range fd");
	state.dma_fd = (int)allocation.fd;
	check_fd(state.dma_fd, "dma-buf", false);
	off_t length = lseek(state.dma_fd, 0, SEEK_END);
	if (length < 0)
		system_fail("DMA-BUF lseek SEEK_END");
	printf("R8-REPRO DMA-BUF length=%jd allocation=%zu width=%d height=%d pitch=%d offset=0 modifier=linear\n",
		(intmax_t)length, state.allocation, SIDE, SIDE, SIDE);
	require((uintmax_t)length == state.allocation && length >= PIXELS,
		"DMA-BUF allocation length mismatch");
	if (lseek(state.dma_fd, 0, SEEK_SET) != 0)
		system_fail("DMA-BUF lseek SEEK_SET");
	state.mapping = mmap(NULL, state.allocation, PROT_READ | PROT_WRITE,
		MAP_SHARED, state.dma_fd, 0);
	if (state.mapping == MAP_FAILED)
		system_fail("mmap DMA-BUF");
	stage("dma-buf-cpu-write.begin");
	dma_sync(DMA_BUF_SYNC_START | DMA_BUF_SYNC_WRITE);
	state.cpu_sync = true;
	memset(state.mapping, 0, state.allocation);
	for (unsigned i = 0; i < PIXELS; i++)
		((uint8_t *)state.mapping)[i] = expected[i][0];
	dma_sync(DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE);
	state.cpu_sync = false;
	stage("dma-buf-cpu-write.end");
	PFNEGLCREATEIMAGEKHRPROC create_image =
		(PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
	egl_ok("eglGetProcAddress eglCreateImageKHR");
	state.destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
	egl_ok("eglGetProcAddress eglDestroyImageKHR");
	PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target =
		(PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
	egl_ok("eglGetProcAddress glEGLImageTargetTexture2DOES");
	require(create_image && state.destroy_image && image_target, "EGL image entrypoints unavailable");
	const EGLint attributes[] = {
		EGL_WIDTH, SIDE, EGL_HEIGHT, SIDE,
		EGL_LINUX_DRM_FOURCC_EXT, DRM_FORMAT_R8,
		EGL_DMA_BUF_PLANE0_FD_EXT, state.dma_fd,
		EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
		EGL_DMA_BUF_PLANE0_PITCH_EXT, SIDE,
		EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, 0,
		EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, 0,
		EGL_NONE,
	};
	stage("r8-egl-import.begin");
	/* The DMA-BUF target requires NO_CONTEXT and a NULL client buffer. */
	state.image = create_image(state.display, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT,
		(EGLClientBuffer)NULL, attributes);
	egl_ok("eglCreateImageKHR R8 linear DMA-BUF");
	require(state.image != EGL_NO_IMAGE_KHR, "R8 DMA-BUF EGL import failed");
	stage("r8-egl-import.end");
	GL(glGenTextures(1, &state.r8_texture));
	require(state.r8_texture != 0, "R8 texture allocation failed");
	GL(glBindTexture(GL_TEXTURE_2D, state.r8_texture));
	texture_parameters();
	stage("r8-image-texture-bind.begin");
	GL(image_target(GL_TEXTURE_2D, (GLeglImageOES)state.image));
	stage("r8-image-texture-bind.end");
	/* No TexImage, swizzle, auxiliary sampler, or conversion after binding. */
}

static bool cleanup_gl(const char *name)
{
	GLenum error = glGetError();
	if (error == GL_NO_ERROR)
		return true;
	fprintf(stderr, "R8-REPRO cleanup=%s GL_error=0x%04x\n", name, error);
	return false;
}

static bool cleanup_egl(EGLBoolean result, const char *name)
{
	EGLint error = eglGetError();
	if (result == EGL_TRUE && error == EGL_SUCCESS)
		return true;
	fprintf(stderr, "R8-REPRO cleanup=%s result=%u EGL_error=0x%04x\n", name, result, error);
	return false;
}

static bool cleanup(void)
{
	bool ok = true;
	stage("cleanup.begin");
	if (state.current) {
		/* Finish before releasing even on a pixel/API failure. */
		glFinish();
		ok &= cleanup_gl("glFinish");
		glBindFramebuffer(GL_FRAMEBUFFER, 0);
		ok &= cleanup_gl("glBindFramebuffer");
		glUseProgram(0);
		ok &= cleanup_gl("glUseProgram");
		glBindTexture(GL_TEXTURE_2D, 0);
		ok &= cleanup_gl("glBindTexture");
		if (state.fbo) {
			glDeleteFramebuffers(1, &state.fbo);
			ok &= cleanup_gl("glDeleteFramebuffers");
		}
		GLuint textures[] = { state.rgba_texture, state.r8_texture, state.target_texture };
		glDeleteTextures(3, textures);
		ok &= cleanup_gl("glDeleteTextures");
		if (state.vbo) {
			glDeleteBuffers(1, &state.vbo);
			ok &= cleanup_gl("glDeleteBuffers");
		}
		if (state.program) {
			glDeleteProgram(state.program);
			ok &= cleanup_gl("glDeleteProgram");
		}
		if (state.vertex) {
			glDeleteShader(state.vertex);
			ok &= cleanup_gl("glDeleteShader vertex");
		}
		if (state.fragment) {
			glDeleteShader(state.fragment);
			ok &= cleanup_gl("glDeleteShader fragment");
		}
	}
	if (state.image != EGL_NO_IMAGE_KHR)
		ok &= cleanup_egl(state.destroy_image(state.display, state.image), "eglDestroyImageKHR");
	if (state.current)
		ok &= cleanup_egl(eglMakeCurrent(state.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT), "eglMakeCurrent release");
	if (state.context != EGL_NO_CONTEXT)
		ok &= cleanup_egl(eglDestroyContext(state.display, state.context), "eglDestroyContext");
	if (state.initialized)
		ok &= cleanup_egl(eglTerminate(state.display), "eglTerminate");
	ok &= cleanup_egl(eglReleaseThread(), "eglReleaseThread");
	if (state.cpu_sync) {
		struct dma_buf_sync sync = { .flags = DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE };
		int result;
		do {
			result = ioctl(state.dma_fd, DMA_BUF_IOCTL_SYNC, &sync);
		} while (result < 0 && (errno == EINTR || errno == EAGAIN));
		if (result != 0) {
			perror("R8-REPRO cleanup DMA_BUF_IOCTL_SYNC");
			ok = false;
		}
	}
	if (state.mapping != MAP_FAILED && munmap(state.mapping, state.allocation) != 0) {
		perror("R8-REPRO cleanup munmap");
		ok = false;
	}
	if (state.gbm)
		gbm_device_destroy(state.gbm);
	int fds[] = { state.dma_fd, state.heap_fd, state.device_fd };
	for (size_t i = 0; i < sizeof(fds) / sizeof(fds[0]); i++) {
		if (fds[i] >= 0 && close(fds[i]) != 0) {
			perror("R8-REPRO cleanup close");
			ok = false;
		}
	}
	printf("R8-REPRO stage=cleanup.end success=%d\n", ok);
	return ok;
}

int main(int argc, char **argv)
{
	if (setvbuf(stdout, NULL, _IONBF, 0) != 0 ||
	    setvbuf(stderr, NULL, _IONBF, 0) != 0) {
		fputs("R8-REPRO FAIL cannot disable output buffering\n", stderr);
		return EXIT_FAILURE;
	}
	const char *device = NULL, *heap = "/dev/dma_heap/system";
	bool heap_given = false;
	for (int i = 1; i < argc; i++) {
		if (!strcmp(argv[i], "--device") && !device && i + 1 < argc)
			device = argv[++i];
		else if (!strcmp(argv[i], "--dma-heap") && !heap_given && i + 1 < argc) {
			heap = argv[++i];
			heap_given = true;
		} else {
			fputs("usage: r8-import --device NODE [--dma-heap /dev/dma_heap/system]\n", stderr);
			return EXIT_FAILURE;
		}
	}
	if (!device || !*device || !*heap) {
		fputs("R8-REPRO FAIL explicit --device and nonempty paths required\n", stderr);
		return EXIT_FAILURE;
	}
	identity("device_path", device);
	identity("dma_heap_path", heap);
	const char *environment[] = {
		"GBM_BACKEND", "LIBGL_ALWAYS_SOFTWARE", "MESA_LOADER_DRIVER_OVERRIDE",
		"GALLIUM_DRIVER", "DRI_PRIME", "EGL_PLATFORM",
	};
	for (size_t i = 0; i < sizeof(environment) / sizeof(environment[0]); i++) {
		const char *value = getenv(environment[i]);
		identity(environment[i], value ? value : "<unset>");
	}
	uint8_t rgba[PIXELS][4], r8[PIXELS][4];
	for (unsigned i = 0; i < PIXELS; i++) {
		rgba[i][0] = (uint8_t)(17 + i * 37);
		rgba[i][1] = (uint8_t)(89 + i * 19);
		rgba[i][2] = (uint8_t)(163 + i * 11);
		rgba[i][3] = 255;
		r8[i][0] = (uint8_t)(i * 255 / (PIXELS - 1));
		r8[i][1] = 0;
		r8[i][2] = 0;
		r8[i][3] = 255;
	}
	create_context(device);
	create_pipeline(rgba);
	draw_and_compare(state.rgba_texture, rgba, "ordinary-rgba-default-shader");
	import_capabilities();
	create_r8(heap, r8);
	draw_and_compare(state.r8_texture, r8, "dma-heap-linear-r8-same-default-shader");
	if (!cleanup())
		return EXIT_FAILURE;
	puts("R8-REPRO PASS rgba_and_r8_all_64_pixels_exact installed_signed_release_qualified=false");
	return EXIT_SUCCESS;
}
