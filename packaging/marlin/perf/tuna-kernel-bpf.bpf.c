/* Native causal DRM observer. Layout is supplied only after live BTF validation. */
#include <linux/bpf.h>
#include <linux/types.h>
#include <bpf/bpf_helpers.h>

#if __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error The observer wire format requires a little-endian target
#endif

#define EVENT_COUNT 10
#define FIELD_COUNT 20

struct layout {
    __u32 crtc_object, crtc_index, crtc_dev, device_primary, minor_index;
    __u32 event_sequence, event_pipe, event_userdata, event_file, timer_member;
    __s32 timer_interval, timer_crtc;
    __u32 hrtimer_expires;
};
struct tally {
    __u64 calls, disabled, emitted, reserve_failed, read_failed;
};
struct record {
    __u64 monotonic_ns, call_sequence;
    __u32 event, cpu;
    __u64 pid_tgid, fields[FIELD_COUNT], present_mask;
    __u32 failed_reads, reserved_zero;
};
_Static_assert(sizeof(struct record) == 208, "schema 1 record size");
_Static_assert(sizeof(struct layout) == 52, "layout wire size");

struct {
    __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
    __uint(max_entries, EVENT_COUNT);
    __type(key, __u32);
    __type(value, struct tally);
} tallies SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __type(key, __u32);
    __type(value, __u32);
} enabled SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_ARRAY);
    __uint(max_entries, 1);
    __uint(map_flags, BPF_F_RDONLY_PROG);
    __type(key, __u32);
    __type(value, struct layout);
} offsets SEC(".maps");
struct {
    __uint(type, BPF_MAP_TYPE_RINGBUF);
    __uint(max_entries, 1024 * 1024);
} records SEC(".maps");

enum field {
    CRTC, OBJECT, PIPE, DEV, MINOR, COMMIT, EVENT, FILE_PTR, USERDATA,
    TARGET, EVENT_PIPE, COUNT, OUTPUT, VALUE, TIMER, INTERVAL, EXPIRES,
    RESTART, SEQUENCE, TIMESTAMP
};

static __always_inline void put(struct record *r, int field, __u64 value)
{
    r->fields[field] = value;
    r->present_mask |= 1ULL << field;
}

/* A failed intermediate pointer read leaves dependent fields absent, not zero-valued. */
static __always_inline int read_value(struct record *r, struct tally *t,
                                     __u64 address, __u32 size, void *value)
{
    if (bpf_probe_read_kernel(value, size, (const void *)address)) {
        r->failed_reads++;
        __sync_fetch_and_add(&t->read_failed, 1);
        return 0;
    }
    return 1;
}

static __always_inline void u32_field(struct record *r, struct tally *t,
                                      int field, __u64 address)
{
    __u32 value = 0;
    if (read_value(r, t, address, sizeof(value), &value))
        put(r, field, value);
}

static __always_inline void s32_field(struct record *r, struct tally *t,
                                      int field, __u64 address)
{
    __s32 value = 0;
    if (read_value(r, t, address, sizeof(value), &value))
        put(r, field, (__s64)value);
}

static __always_inline void u64_field(struct record *r, struct tally *t,
                                      int field, __u64 address)
{
    __u64 value = 0;
    if (read_value(r, t, address, sizeof(value), &value))
        put(r, field, value);
}

static __always_inline void minor_fields(struct record *r, struct tally *t,
                                         const struct layout *o, __u64 dev)
{
    __u64 primary = 0;
    if (read_value(r, t, dev + o->device_primary, sizeof(primary), &primary))
        s32_field(r, t, MINOR, primary + o->minor_index);
}

static __always_inline void crtc_fields(struct record *r, struct tally *t,
                                        const struct layout *o, __u64 crtc)
{
    __u64 dev = 0;
    put(r, CRTC, crtc);
    u32_field(r, t, OBJECT, crtc + o->crtc_object);
    u32_field(r, t, PIPE, crtc + o->crtc_index);
    if (read_value(r, t, crtc + o->crtc_dev, sizeof(dev), &dev)) {
        put(r, DEV, dev);
        minor_fields(r, t, o, dev);
    }
}

static __always_inline void pending_fields(struct record *r, struct tally *t,
                                           const struct layout *o, __u64 event)
{
    put(r, EVENT, event);
    u64_field(r, t, FILE_PTR, event + o->event_file);
    u64_field(r, t, USERDATA, event + o->event_userdata);
}

static __always_inline int observe(__u64 *ctx, __u32 id)
{
    __u32 zero = 0, key = id;
    struct tally *t = bpf_map_lookup_elem(&tallies, &key);
    __u32 *on = bpf_map_lookup_elem(&enabled, &zero);
    struct layout *o = bpf_map_lookup_elem(&offsets, &zero);
    if (!t)
        return 0; /* Impossible for the fixed, validated array inventory. */
    __u64 call = __sync_fetch_and_add(&t->calls, 1) + 1;
    if (!on || !*on) {
        __sync_fetch_and_add(&t->disabled, 1);
        return 0;
    }
    struct record *r = bpf_ringbuf_reserve(&records, sizeof(*r), 0);
    if (!r) {
        __sync_fetch_and_add(&t->reserve_failed, 1);
        return 0;
    }
    __builtin_memset(r, 0, sizeof(*r));
    r->monotonic_ns = bpf_ktime_get_ns();
    r->call_sequence = call;
    r->event = id;
    r->cpu = bpf_get_smp_processor_id();
    r->pid_tgid = bpf_get_current_pid_tgid();
    if (!o) {
        /* Still retain a rejected record and reconcile the call. */
        r->failed_reads++;
        __sync_fetch_and_add(&t->read_failed, 1);
    } else if (id == 7 || id == 8) {
        __u64 timer = ctx[0], crtc = 0;
        put(r, TIMER, timer);
        int has_crtc = read_value(r, t, timer + (__s64)o->timer_crtc, sizeof(crtc), &crtc);
        u64_field(r, t, INTERVAL, timer + (__s64)o->timer_interval);
        u64_field(r, t, EXPIRES, timer + o->hrtimer_expires);
        if (has_crtc)
            crtc_fields(r, t, o, crtc);
        if (id == 8)
            put(r, RESTART, (__s64)(__s32)ctx[1]);
    } else if (id == 9) {
        put(r, DEV, ctx[0]);
        minor_fields(r, t, o, ctx[0]);
        pending_fields(r, t, o, ctx[1]);
        put(r, SEQUENCE, ctx[2]);
        put(r, TIMESTAMP, ctx[3]);
        u32_field(r, t, EVENT_PIPE, ctx[1] + o->event_pipe);
    } else {
        crtc_fields(r, t, o, ctx[0]);
        if (id == 0)
            put(r, COMMIT, ctx[1]);
        if (id == 1 || id == 2) {
            pending_fields(r, t, o, ctx[1]);
            if (id == 2) {
                u64_field(r, t, TARGET, ctx[1] + o->event_sequence);
                u32_field(r, t, EVENT_PIPE, ctx[1] + o->event_pipe);
            }
        }
        if (id == 4)
            put(r, COUNT, ctx[1]);
        if (id == 5 || id == 6) {
            put(r, OUTPUT, ctx[1]);
            if (id == 6)
                u64_field(r, t, VALUE, ctx[1]);
        }
    }
    bpf_ringbuf_submit(r, 0);
    __sync_fetch_and_add(&t->emitted, 1);
    return 0;
}

#define OBSERVE(symbol, section_name, id) \
    SEC(section_name) int symbol(__u64 *ctx) { return observe(ctx, id); }
OBSERVE(tuna_flush, "fentry/virtio_gpu_crtc_atomic_flush", 0)
OBSERVE(tuna_arm, "fentry/drm_crtc_arm_vblank_event", 1)
OBSERVE(tuna_arm_x, "fexit/drm_crtc_arm_vblank_event", 2)
OBSERVE(tuna_accurate, "fentry/drm_crtc_accurate_vblank_count", 3)
OBSERVE(tuna_accurate_x, "fexit/drm_crtc_accurate_vblank_count", 4)
OBSERVE(tuna_timeout, "fentry/drm_crtc_vblank_get_vblank_timeout", 5)
OBSERVE(tuna_timeout_x, "fexit/drm_crtc_vblank_get_vblank_timeout", 6)
OBSERVE(tuna_timer, "fentry/drm_vblank_timer_function", 7)
OBSERVE(tuna_timer_x, "fexit/drm_vblank_timer_function", 8)
OBSERVE(tuna_send, "fentry/send_vblank_event", 9)
char LICENSE[] SEC("license") = "GPL";
