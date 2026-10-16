/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Private audio-proof recorder; no disk I/O in the PipeWire process callback.
 * API shape follows PipeWire 1.0.5 audio-capture.c (MIT, Wim Taymans).
 */
#include <pipewire/pipewire.h>
#include <spa/param/audio/format-utils.h>
#include <spa/buffer/meta.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <signal.h>
#define CAPACITY (48000u * 60u * 4u)
struct capture {
    struct pw_main_loop *loop;
    struct pw_stream *stream;
    uint8_t *samples;
    _Atomic size_t bytes;
    _Atomic unsigned callbacks, invalid, overflow;
    unsigned errors;
};
static void process(void *arg) {
    struct capture *c = arg;
    struct pw_buffer *b;
    while ((b = pw_stream_dequeue_buffer(c->stream)) != NULL) {
        struct spa_buffer *s = b->buffer;
        if (s->n_datas != 1 || !s->datas[0].data || !s->datas[0].chunk) {
            atomic_fetch_add(&c->invalid, 1);
        } else {
            struct spa_data *d = &s->datas[0];
            uint32_t off = d->chunk->offset, count = d->chunk->size;
            size_t pos = atomic_load(&c->bytes);
            if (off > d->maxsize || count > d->maxsize - off || count % 4 != 0) {
                atomic_fetch_add(&c->invalid, 1);
            } else if (count > CAPACITY - pos) {
                atomic_fetch_add(&c->overflow, 1);
            } else {
                memcpy(c->samples + pos, (uint8_t *)d->data + off, count);
                atomic_store(&c->bytes, pos + count);
                atomic_fetch_add(&c->callbacks, 1);
            }
        }
        pw_stream_queue_buffer(c->stream, b);
    }
}
static void param(void *arg, uint32_t id, const struct spa_pod *p) {
    struct capture *c = arg;
    struct spa_audio_info_raw f = {0};
    if (id != SPA_PARAM_Format || p == NULL) return;
    if (spa_format_audio_raw_parse(p, &f) < 0 || f.format != SPA_AUDIO_FORMAT_S16_LE || f.rate != 48000 || f.channels != 2) {
        c->errors++;
        pw_main_loop_quit(c->loop);
    }
}
static void state(void *arg, enum pw_stream_state old, enum pw_stream_state now, const char *error) {
    struct capture *c = arg;
    (void)old;
    if (now == PW_STREAM_STATE_ERROR) {
        fprintf(stderr, "capture error: %s\n", error ? error : "unknown");
        c->errors++;
        pw_main_loop_quit(c->loop);
    }
}
static const struct pw_stream_events events = {
    PW_VERSION_STREAM_EVENTS,
    .state_changed = state,
    .param_changed = param,
    .process = process,
};
static void quit(void *arg, int sig) {
    (void)sig;
    struct capture *c = arg;
    pw_main_loop_quit(c->loop);
}
static void le16(uint8_t *p, uint16_t n) { p[0] = n; p[1] = n >> 8; }
static void le32(uint8_t *p, uint32_t n) { for (unsigned i = 0; i < 4; i++) p[i] = n >> (8*i); }
int main(int argc, char **argv) {
    if (argc != 4) { fprintf(stderr, "usage: recorder TARGET WAV LATENCY_SAMPLES\n"); return 2; }
    char *end;
    unsigned long latency = strtoul(argv[3], &end, 10);
    if (*end || latency < 1 || latency > 8192) return 2;
    struct capture c = {0};
    c.samples = calloc(1, CAPACITY);
    if (!c.samples) return 2;
    /* Fault in all history pages before connecting to the graph. */
    for (size_t i = 0; i < CAPACITY; i += 4096) ((volatile uint8_t *)c.samples)[i] = 0;
    pw_init(&argc, &argv);
    c.loop = pw_main_loop_new(NULL);
    if (!c.loop) { free(c.samples); return 2; }
    pw_loop_add_signal(pw_main_loop_get_loop(c.loop), SIGINT, quit, &c);
    pw_loop_add_signal(pw_main_loop_get_loop(c.loop), SIGTERM, quit, &c);
    struct pw_properties *props = pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Audio", PW_KEY_CONFIG_NAME, "client-rt.conf",
        PW_KEY_MEDIA_CATEGORY, "Capture", PW_KEY_MEDIA_ROLE, "Music",
        PW_KEY_NODE_NAME, "tuna-sound-proof-recorder", PW_KEY_TARGET_OBJECT, argv[1],
        PW_KEY_STREAM_CAPTURE_SINK, "true", PW_KEY_NODE_PASSIVE, "true", NULL);
    pw_properties_setf(props, PW_KEY_NODE_LATENCY, "%lu/48000", latency);
    c.stream = pw_stream_new_simple(pw_main_loop_get_loop(c.loop), "tuna-proof-memory-recorder", props, &events, &c);
    if (!c.stream) { pw_main_loop_destroy(c.loop); free(c.samples); return 2; }
    uint8_t pod_bytes[1024];
    struct spa_pod_builder builder = SPA_POD_BUILDER_INIT(pod_bytes, sizeof(pod_bytes));
    const struct spa_pod *pods[] = {spa_format_audio_raw_build(&builder, SPA_PARAM_EnumFormat,
        &SPA_AUDIO_INFO_RAW_INIT(.format = SPA_AUDIO_FORMAT_S16_LE, .rate = 48000, .channels = 2,
                                .position = { SPA_AUDIO_CHANNEL_FL, SPA_AUDIO_CHANNEL_FR }))};
    if (pw_stream_connect(c.stream, PW_DIRECTION_INPUT, PW_ID_ANY,
        PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS | PW_STREAM_FLAG_RT_PROCESS, pods, 1) < 0) c.errors++;
    else pw_main_loop_run(c.loop);
    pw_stream_disconnect(c.stream);
    pw_stream_destroy(c.stream);
    pw_main_loop_destroy(c.loop);
    pw_deinit();
    size_t count = atomic_load(&c.bytes);
    uint8_t header[44] = {0};
    memcpy(header, "RIFF", 4); le32(header+4, 36+count); memcpy(header+8, "WAVEfmt ", 8);
    le32(header+16, 16); le16(header+20, 1); le16(header+22, 2); le32(header+24, 48000);
    le32(header+28, 48000*4); le16(header+32, 4); le16(header+34, 16);
    memcpy(header+36, "data", 4); le32(header+40, count);
    FILE *f = fopen(argv[2], "wb");
    if (!f) c.errors++;
    else {
        if (fwrite(header, 1, sizeof(header), f) != sizeof(header) || fwrite(c.samples, 1, count, f) != count) c.errors++;
        if (fclose(f) != 0) c.errors++;
    }
    fprintf(stderr, "{\"frames\":%zu,\"callbacks\":%u,\"invalid_buffers\":%u,\"overflow_buffers\":%u,\"errors\":%u}\n",
        count/4, atomic_load(&c.callbacks), atomic_load(&c.invalid), atomic_load(&c.overflow), c.errors);
    int failed = c.errors || atomic_load(&c.invalid) || atomic_load(&c.overflow);
    free(c.samples);
    return failed ? 1 : 0;
}
