/* Fixed-purpose, loss-intolerant native DRM observer; no pins or fallback backend. */
#define _GNU_SOURCE
#include <bpf/bpf.h>
#include <bpf/btf.h>
#include <bpf/libbpf.h>
#include <linux/bpf.h>
#include <linux/btf.h>
#include <linux/magic.h>
#include <endian.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <limits.h>
#include <signal.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/vfs.h>
#include <time.h>
#include <unistd.h>

#if __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__
#error The observer wire format requires a little-endian host
#endif
#define EVENTS 10
#define CPUS 8
#define CPU_ID_LIMIT 65536U
/* libbpf's possible-CPU parser itself requires this sysfs list to fit128 bytes. */
#define CPU_LIST_LIMIT 128U
#define RING_BYTES (1024U * 1024U)
#define RAW_LIMIT (64ULL * 1024 * 1024)
#define MAX_LIFETIME_NS (120ULL * 1000000000)
#define RECEIPT_BYTES 32768
#define BIT(n) (1ULL << (n))
#define BARRIER_LINE_BYTES 1024
#define ENABLED_FUNCTIONS_LIMIT (4U * 1024U * 1024U)
static const char barrier_function[] = "drm_crtc_arm_vblank_event";

struct layout {
    uint32_t crtc_object, crtc_index, crtc_dev, device_primary, minor_index;
    uint32_t event_sequence, event_pipe, event_userdata, event_file, timer_member;
    int32_t timer_interval, timer_crtc;
    uint32_t hrtimer_expires;
};
struct tally { uint64_t calls, disabled, emitted, reserve_failed, read_failed; };
struct record {
    uint64_t monotonic_ns, call_sequence;
    uint32_t event, cpu;
    uint64_t pid_tgid, fields[20], present_mask;
    uint32_t failed_reads, reserved_zero;
};
_Static_assert(sizeof(struct layout) == 13 * 4, "layout ABI");
_Static_assert(sizeof(struct record) == 208, "record ABI");
_Static_assert(offsetof(struct record, fields) == 32, "fields ABI");
_Static_assert(offsetof(struct record, present_mask) == 192, "presence ABI");
_Static_assert(sizeof(struct tally) == 40, "per-CPU map alignment");
_Static_assert(sizeof(unsigned long) == 8, "64-bit ring ABI required");

struct site {
    const char *name, *symbol, *function, *section;
    enum bpf_attach_type attach_type;
    uint64_t mask;
};
#define CRTC_MASK (BIT(0)|BIT(1)|BIT(2)|BIT(3)|BIT(4))
#define PENDING_MASK (BIT(6)|BIT(7)|BIT(8))
#define TIMER_MASK (CRTC_MASK|BIT(14)|BIT(15)|BIT(16))
static const struct site sites[EVENTS] = {
    {"flush", "tuna_flush", "virtio_gpu_crtc_atomic_flush", "fentry/virtio_gpu_crtc_atomic_flush", BPF_TRACE_FENTRY, CRTC_MASK|BIT(5)},
    {"arm", "tuna_arm", "drm_crtc_arm_vblank_event", "fentry/drm_crtc_arm_vblank_event", BPF_TRACE_FENTRY, CRTC_MASK|PENDING_MASK},
    {"arm_return", "tuna_arm_x", "drm_crtc_arm_vblank_event", "fexit/drm_crtc_arm_vblank_event", BPF_TRACE_FEXIT, CRTC_MASK|PENDING_MASK|BIT(9)|BIT(10)},
    {"accurate", "tuna_accurate", "drm_crtc_accurate_vblank_count", "fentry/drm_crtc_accurate_vblank_count", BPF_TRACE_FENTRY, CRTC_MASK},
    {"accurate_return", "tuna_accurate_x", "drm_crtc_accurate_vblank_count", "fexit/drm_crtc_accurate_vblank_count", BPF_TRACE_FEXIT, CRTC_MASK|BIT(11)},
    {"timeout", "tuna_timeout", "drm_crtc_vblank_get_vblank_timeout", "fentry/drm_crtc_vblank_get_vblank_timeout", BPF_TRACE_FENTRY, CRTC_MASK|BIT(12)},
    {"timeout_return", "tuna_timeout_x", "drm_crtc_vblank_get_vblank_timeout", "fexit/drm_crtc_vblank_get_vblank_timeout", BPF_TRACE_FEXIT, CRTC_MASK|BIT(12)|BIT(13)},
    {"timer", "tuna_timer", "drm_vblank_timer_function", "fentry/drm_vblank_timer_function", BPF_TRACE_FENTRY, TIMER_MASK},
    {"timer_return", "tuna_timer_x", "drm_vblank_timer_function", "fexit/drm_vblank_timer_function", BPF_TRACE_FEXIT, TIMER_MASK|BIT(17)},
    {"send", "tuna_send", "send_vblank_event", "fentry/send_vblank_event", BPF_TRACE_FENTRY, BIT(3)|BIT(4)|PENDING_MASK|BIT(10)|BIT(18)|BIT(19)},
};

/* Error text is fixed; the only kernel text retained is bounded registration evidence. */
enum error {
    E_ARGUMENTS, E_CLOCK, E_DEADLINE, E_CPU, E_SIGNALS, E_LAYOUT, E_OBJECT,
    E_INVENTORY, E_LOAD, E_MAPS, E_LAYOUT_MAP_SETUP, E_STATS, E_INFO, E_OWNER, E_ATTACH,
    E_RING_MAP, E_RAW_OPEN, E_READY, E_CONTROL, E_EARLY_STOP, E_POLL, E_RING,
    E_DISCARD, E_RECORD, E_SEQUENCE, E_TIMESTAMP, E_RAW_LIMIT, E_RAW_WRITE,
    E_READ, E_TALLY, E_MISSES, E_RUN_MONOTONIC, E_RUN_CALLS, E_CALL_BALANCE,
    E_RESERVE, E_EMITTED, E_READ_BALANCE, E_EMPTY, E_RESULT,
    E_BARRIER_CONFIG, E_BARRIER, ERROR_COUNT
};
static const char *const error_names[ERROR_COUNT] = {
    "invalid_arguments", "monotonic_clock_failed", "absolute_deadline_exceeded",
    "cpu_inventory_out_of_bounds", "signal_setup_failed", "invalid_layout_file",
    "invalid_bpf_object", "program_inventory_mismatch", "bpf_object_load_failed",
    "map_inventory_mismatch", "immutable_layout_map_setup_failed", "runtime_stats_unavailable",
    "program_counters_unreadable", "attach_btf_identity_mismatch", "program_attach_failed",
    "ring_mapping_failed", "raw_file_open_failed", "ready_receipt_failed",
    "control_update_failed", "stop_before_ready", "ring_poll_failed",
    "malformed_or_incomplete_ring", "unexpected_discarded_record", "malformed_record",
    "per_cpu_call_sequence_mismatch", "record_timestamp_outside_envelope",
    "raw_hard_cap_exceeded", "raw_write_failed", "kernel_read_failed",
    "per_cpu_tallies_unreadable", "recursion_misses_nonzero", "program_counters_regressed",
    "run_count_call_tally_mismatch", "call_tally_balance_mismatch", "ring_reserve_failed",
    "emitted_retained_mismatch", "read_failure_tally_mismatch", "no_retained_events",
    "result_receipt_failed", "invalid_quiescence_instance",
    "tasks_rcu_quiescence_failed"
};
enum cleanup {
    C_DISABLE, C_DETACH, C_CLOSE, C_UNMAP, C_RAW_SYNC, C_RECEIPT, C_DIRECTORY,
    C_BARRIER_RESET, CLEANUP_COUNT
};
static const char *const cleanup_names[CLEANUP_COUNT] = {
    "control_disable_failed", "physical_detach_failed", "file_descriptor_close_failed",
    "mapping_release_failed", "raw_fsync_failed", "receipt_cleanup_failed",
    "directory_fsync_failed", "quiescence_tracer_reset_failed"
};
struct owner {
    uint32_t id;
    const char *name;
    struct btf *btf;
};
struct registration {
    bool readable, found;
    uint32_t count;
    size_t length;
    char line[BARRIER_LINE_BYTES];
};
struct state {
    int root, raw, stats, control, tally_fd, ring_fd, object_fd;
    int barrier, trace_root;
    unsigned long barrier_buffer_kb;
    uint64_t quiescence_begin, quiescence_end;
    bool barrier_valid, quiescence_completed;
    struct registration registration[3];
    int cpus, possible_cpus;
    uint32_t online_ids[CPUS], online_possible_slots[CPUS];
    struct tally *counter_scratch;
    size_t counter_scratch_bytes;
    size_t page;
    void *object_data;
    size_t object_size;
    struct bpf_object *object;
    struct bpf_program *programs[EVENTS];
    struct bpf_link *links[EVENTS];
    struct bpf_prog_info before[EVENTS], after[EVENTS];
    bool before_valid[EVENTS], after_valid[EVENTS];
    struct tally totals[EVENTS];
    uint64_t retained[EVENTS], retained_cpu[EVENTS][CPUS];
    uint64_t read_failures[EVENTS], last_call[EVENTS][CPUS];
    uint64_t last_time[EVENTS][CPUS], max_record_time;
    struct ring_buffer *ring;
    unsigned int callback_batch;
    uint64_t errors, cleanup_errors, raw_bytes, deadline;
    uint64_t enable_begin, started, stop_begin, stopped, detached;
    struct btf *base;
    struct owner owners[3];
    const char *drm_owner;
    bool loaded, enabled, raw_broken, cap_reached, capture_ready;
    char receipt[RECEIPT_BYTES];
    size_t receipt_used;
};
static struct state state;
static volatile sig_atomic_t stop_signal;

static void stop_handler(int number) { stop_signal = number; }
static void fail(struct state *s, enum error e) { s->errors |= BIT(e); }
static void cleanup_fail(struct state *s, enum cleanup e) { s->cleanup_errors |= BIT(e); }
static uint64_t now_ns(struct state *s)
{
    struct timespec time;
    if (clock_gettime(CLOCK_MONOTONIC, &time)) {
        fail(s, E_CLOCK);
        return 0;
    }
    return (uint64_t)time.tv_sec * 1000000000 + (uint64_t)time.tv_nsec;
}
static bool in_time(struct state *s)
{
    uint64_t now = now_ns(s);
    if (!now || now >= s->deadline) {
        fail(s, E_DEADLINE);
        return false;
    }
    return true;
}
static void close_checked(struct state *s, int *fd)
{
    if (*fd >= 0) {
        /* On Linux even EINTR closes the descriptor; never retry a close. */
        if (close(*fd))
            cleanup_fail(s, C_CLOSE);
        *fd = -1;
    }
}

/* Walk all components, not only the last: an output path may contain no symlinks. */
static int open_directory(struct state *s, const char *path)
{
    char copy[PATH_MAX], *save = NULL;
    if (!path || path[0] != '/' || strlen(path) >= sizeof(copy))
        return -1;
    memcpy(copy, path, strlen(path) + 1);
    int fd = open("/", O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
    if (fd < 0)
        return -1;
    for (char *part = strtok_r(copy, "/", &save); part; part = strtok_r(NULL, "/", &save)) {
        if (!strcmp(part, ".") || !strcmp(part, "..")) {
            close_checked(s, &fd);
            return -1;
        }
        int next = openat(fd, part, O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
        close_checked(s, &fd);
        if (next < 0)
            return -1;
        fd = next;
    }
    struct stat info;
    if (fstat(fd, &info) || !S_ISDIR(info.st_mode) || info.st_uid != 0 ||
        (info.st_mode & 07777) != 0700) {
        close_checked(s, &fd);
        return -1;
    }
    return fd;
}
static bool read_cpu_list(struct state *s, const char *path, char *text, size_t capacity)
{
    int fd = open(path, O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
    if (fd < 0)
        return false;
    size_t used = 0;
    bool ok = true;
    while (used < capacity) {
        ssize_t got = read(fd, text + used, capacity - used);
        if (got < 0 && errno == EINTR)
            continue;
        if (got < 0) { ok = false; break; }
        if (!got)
            break;
        used += (size_t)got;
    }
    close_checked(s, &fd);
    if (!ok || !used || used == capacity)
        return false;
    text[used] = '\0';
    return true;
}
static bool cpu_number(const char **cursor, uint32_t *value)
{
    const char *p = *cursor;
    uint32_t n = 0;
    if (*p < '0' || *p > '9')
        return false;
    do {
        if (n > (CPU_ID_LIMIT - 1 - (unsigned int)(*p - '0')) / 10)
            return false;
        n = n * 10 + (unsigned int)(*p++ - '0');
    } while (*p >= '0' && *p <= '9');
    *cursor = p;
    *value = n;
    return true;
}
static bool parse_cpu_list(const char *text, uint32_t *ids, size_t capacity, size_t *count)
{
    size_t n = 0;
    const char *p = text;
    for (;;) {
        uint32_t first, last;
        if (!cpu_number(&p, &first))
            return false;
        last = first;
        if (*p == '-') {
            p++;
            if (!cpu_number(&p, &last) || last < first)
                return false;
        }
        if ((n && first <= ids[n - 1]) || (size_t)(last - first + 1) > capacity - n)
            return false;
        for (uint32_t cpu = first; cpu <= last; cpu++)
            ids[n++] = cpu;
        if (*p != ',')
            break;
        p++;
    }
    if (*p == '\n')
        p++;
    if (*p || !n)
        return false;
    *count = n;
    return true;
}
static bool prepare_cpus(struct state *s, long online_count)
{
    s->possible_cpus = libbpf_num_possible_cpus();
    if (s->possible_cpus < 1 || s->possible_cpus > (int)CPU_ID_LIMIT ||
        online_count < 1 || online_count > CPUS)
        return false;
    s->counter_scratch_bytes = (size_t)s->possible_cpus * sizeof(struct tally);
    s->counter_scratch = malloc(s->counter_scratch_bytes);
    char text[CPU_LIST_LIMIT + 1];
    if (!s->counter_scratch) {
        return false;
    }
    /* Before its first lookup, the same allocation temporarily holds possible IDs. */
    uint32_t *possible_ids = (void *)s->counter_scratch;
    size_t possible = 0, online = 0;
    bool ok = read_cpu_list(s, "/sys/devices/system/cpu/possible", text, CPU_LIST_LIMIT) &&
        parse_cpu_list(text, possible_ids, (size_t)s->possible_cpus, &possible) &&
        possible == (size_t)s->possible_cpus &&
        read_cpu_list(s, "/sys/devices/system/cpu/online", text, CPU_LIST_LIMIT) &&
        parse_cpu_list(text, s->online_ids, CPUS, &online) && online == (size_t)online_count;
    if (ok) {
        size_t p = 0;
        for (size_t c = 0; c < online; c++) {
            while (p < possible && possible_ids[p] < s->online_ids[c])
                p++;
            if (p == possible || possible_ids[p] != s->online_ids[c]) {
                ok = false;
                break;
            }
            s->online_possible_slots[c] = (uint32_t)p;
        }
        if (ok)
            s->cpus = (int)online;
    }
    return ok;
}
static bool online_cpus_unchanged(struct state *s)
{
    char text[128];
    uint32_t ids[CPUS];
    size_t count = 0;
    return read_cpu_list(s, "/sys/devices/system/cpu/online", text, sizeof(text) - 1) &&
        parse_cpu_list(text, ids, CPUS, &count) && count == (size_t)s->cpus &&
        !memcmp(ids, s->online_ids, count * sizeof(ids[0]));
}
static bool private_file(int fd, off_t extent)
{
    struct stat info;
    return fd >= 0 && !fstat(fd, &info) && S_ISREG(info.st_mode) &&
        info.st_uid == 0 && info.st_nlink == 1 && !(info.st_mode & 0077) &&
        info.st_size == extent;
}
static bool load_layout(struct state *s, struct layout *layout)
{
    int fd = openat(s->root, "layout.bin", O_RDONLY|O_NOFOLLOW|O_CLOEXEC|O_NONBLOCK);
    bool valid = private_file(fd, sizeof(*layout));
    if (valid) {
        ssize_t got;
        do { got = pread(fd, layout, sizeof(*layout), 0); } while (got < 0 && errno == EINTR && !stop_signal);
        valid = got == (ssize_t)sizeof(*layout);
    }
    close_checked(s, &fd);
    if (!valid)
        return false;
    /* Field widths are immutable ABI constants; BTF type/extent validation is the launcher's job. */
    const uint32_t values[] = {
        layout->crtc_object, layout->crtc_index, layout->crtc_dev,
        layout->device_primary, layout->minor_index, layout->event_sequence,
        layout->event_pipe, layout->event_userdata, layout->event_file,
        layout->timer_member, layout->hrtimer_expires
    };
    for (size_t i = 0; i < sizeof(values)/sizeof(values[0]); i++)
        if (values[i] > INT32_MAX - 8U)
            return false;
    int64_t interval = (int64_t)layout->timer_member + layout->timer_interval;
    int64_t crtc = (int64_t)layout->timer_member + layout->timer_crtc;
    return interval >= 0 && crtc >= 0 && interval <= INT32_MAX - 8 && crtc <= INT32_MAX - 8;
}
static bool map_matches(int fd, enum bpf_map_type type, uint32_t key_size,
                        uint32_t value_size, uint32_t entries, uint32_t flags)
{
    struct bpf_map_info info = {0};
    uint32_t size = sizeof(info);
    return fd >= 0 && !bpf_map_get_info_by_fd(fd, &info, &size) &&
        size >= offsetof(struct bpf_map_info, map_flags) + sizeof(info.map_flags) &&
        info.type == (uint32_t)type && info.key_size == key_size &&
        info.value_size == value_size && info.max_entries == entries && info.map_flags == flags;
}
static bool program_info(struct state *s, int event, struct bpf_prog_info *info)
{
    memset(info, 0, sizeof(*info));
    uint32_t size = sizeof(*info);
    if (bpf_prog_get_info_by_fd(bpf_program__fd(s->programs[event]), info, &size) ||
        size < offsetof(struct bpf_prog_info, recursion_misses) + sizeof(info->recursion_misses) ||
        size < offsetof(struct bpf_prog_info, attach_btf_id) + sizeof(info->attach_btf_id)) {
        fail(s, E_INFO);
        return false;
    }
    if (info->type != BPF_PROG_TYPE_TRACING || !info->id ||
        strncmp(info->name, sites[event].symbol, sizeof(info->name))) {
        fail(s, E_INVENTORY);
        return false;
    }
    return true;
}
static bool verify_owner(struct state *s, int event, const struct bpf_prog_info *info)
{
    const char *wanted = event == 0 ? "virtio_gpu" : s->drm_owner;
    int slot = event == 0 ? 0 : (!strcmp(wanted, "vmlinux") ? 1 : 2);
    struct owner *owner = &s->owners[slot];
    if (!info->attach_btf_obj_id || !info->attach_btf_id)
        return false;
    if (!owner->btf) {
        int fd = bpf_btf_get_fd_by_id(info->attach_btf_obj_id);
        char name[64] = {0};
        struct bpf_btf_info btf_info = {
            .name = (uint64_t)(uintptr_t)name, .name_len = sizeof(name)
        };
        uint32_t size = sizeof(btf_info);
        bool valid = fd >= 0 && !bpf_btf_get_info_by_fd(fd, &btf_info, &size) &&
            size >= offsetof(struct bpf_btf_info, kernel_btf) + sizeof(btf_info.kernel_btf) &&
            btf_info.kernel_btf && btf_info.id == info->attach_btf_obj_id &&
            btf_info.name_len < sizeof(name) && !strcmp(name, wanted);
        close_checked(s, &fd);
        if (!valid)
            return false;
        if (strcmp(wanted, "vmlinux") && !s->base) {
            s->base = btf__load_vmlinux_btf();
            if (libbpf_get_error(s->base)) {
                s->base = NULL;
                return false;
            }
        }
        owner->btf = btf__load_from_kernel_by_id_split(info->attach_btf_obj_id,
                         strcmp(wanted, "vmlinux") ? s->base : NULL);
        if (libbpf_get_error(owner->btf)) {
            owner->btf = NULL;
            return false;
        }
        owner->id = info->attach_btf_obj_id;
        owner->name = wanted;
    }
    if (owner->id != info->attach_btf_obj_id)
        return false;
    const struct btf_type *type = btf__type_by_id(owner->btf, info->attach_btf_id);
    if (!type || btf_kind(type) != BTF_KIND_FUNC ||
        (strcmp(wanted, "vmlinux") && info->attach_btf_id < btf__type_cnt(s->base)))
        return false;
    const char *name = btf__name_by_offset(owner->btf, type->name_off);
    return name && !strcmp(name, sites[event].function);
}

static void retain(struct state *s, const void *data, size_t size)
{
    if (s->raw_broken || s->cap_reached)
        return;
    if (size > RAW_LIMIT - s->raw_bytes) {
        s->cap_reached = true;
        fail(s, E_RAW_LIMIT);
        return;
    }
    const unsigned char *p = data;
    size_t remaining = size;
    while (remaining) {
        ssize_t written = write(s->raw, p, remaining);
        if (written < 0 && errno == EINTR)
            continue;
        if (written <= 0) {
            s->raw_broken = true;
            fail(s, E_RAW_WRITE);
            return;
        }
        p += written;
        remaining -= (size_t)written;
        s->raw_bytes += (uint64_t)written;
    }
}
static void event_record(struct state *s, const void *data, size_t size)
{
    uint64_t previous_bytes = s->raw_bytes;
    /* Preserve malformed events too; semantic rejection never trims the raw stream. */
    retain(s, data, size);
    if (size != sizeof(struct record)) {
        fail(s, E_RECORD);
        return;
    }
    const struct record *r = data;
    int slot = -1;
    for (int c = 0; c < s->cpus; c++)
        if (s->online_ids[c] == r->cpu) { slot = c; break; }
    if (r->event >= EVENTS || slot < 0) {
        fail(s, E_RECORD);
        if (slot < 0) fail(s, E_CPU);
        return;
    }
    unsigned int e = r->event, c = (unsigned int)slot;
    if (s->raw_bytes - previous_bytes == size) {
        s->retained[e]++;
        s->retained_cpu[e][c]++;
        s->read_failures[e] += r->failed_reads;
    }
    if (r->reserved_zero || (r->present_mask & ~sites[e].mask) ||
        (!r->failed_reads && r->present_mask != sites[e].mask))
        fail(s, E_RECORD);
    for (unsigned int i = 0; i < 20; i++)
        if (!(r->present_mask & BIT(i)) && r->fields[i])
            fail(s, E_RECORD);
    if (((r->present_mask & BIT(1)) && r->fields[1] > UINT32_MAX) ||
        ((r->present_mask & BIT(2)) && r->fields[2] > UINT32_MAX) ||
        ((r->present_mask & BIT(10)) && r->fields[10] > UINT32_MAX) ||
        ((r->present_mask & BIT(4)) && r->fields[4] != (uint64_t)(int64_t)(int32_t)r->fields[4]) ||
        ((r->present_mask & BIT(17)) && r->fields[17] != (uint64_t)(int64_t)(int32_t)r->fields[17]))
        fail(s, E_RECORD);
    if (r->failed_reads)
        fail(s, E_READ);
    if (!r->call_sequence || (s->last_call[e][c] && r->call_sequence != s->last_call[e][c] + 1))
        fail(s, E_SEQUENCE);
    if (!r->monotonic_ns || r->monotonic_ns < s->enable_begin ||
        (s->last_time[e][c] && r->monotonic_ns < s->last_time[e][c]))
        fail(s, E_TIMESTAMP);
    s->last_call[e][c] = r->call_sequence;
    s->last_time[e][c] = r->monotonic_ns;
    if (r->monotonic_ns > s->max_record_time)
        s->max_record_time = r->monotonic_ns;
}
/* Bound each libbpf consume pass even when producers are continuously active. */
static int ring_event(void *context, void *data, size_t size)
{
    struct state *s = context;
    event_record(s, data, size);
    if (++s->callback_batch == 256) {
        s->callback_batch = 0;
        return -EINTR; /* libbpf advances the consumer before returning this value. */
    }
    return 0;
}

static bool append(struct state *s, const char *format, ...)
{
    if (s->receipt_used >= sizeof(s->receipt))
        return false;
    va_list args;
    va_start(args, format);
    int n = vsnprintf(s->receipt + s->receipt_used, sizeof(s->receipt) - s->receipt_used, format, args);
    va_end(args);
    if (n < 0 || (size_t)n >= sizeof(s->receipt) - s->receipt_used) {
        s->receipt_used = sizeof(s->receipt);
        return false;
    }
    s->receipt_used += (size_t)n;
    return true;
}
static void append_programs(struct state *s, bool after)
{
    append(s, "[");
    for (int e = 0; e < EVENTS; e++) {
        bool valid = after ? s->after_valid[e] : s->before_valid[e];
        if (!valid) {
            /* Partial startup results cannot masquerade as a complete metadata inventory. */
            append(s, "%snull", e ? "," : "");
            continue;
        }
        const struct bpf_prog_info *p = after ? &s->after[e] : &s->before[e];
        append(s, "%s{\"event\":%d,\"name\":\"%s\",\"id\":%u,"
               "\"attach_btf_obj_id\":%u,\"attach_btf_id\":%u,\"attach_owner\":\"%s\","
               "\"run_cnt\":%" PRIu64 ",\"recursion_misses\":%" PRIu64 ",\"run_time_ns\":%" PRIu64,
               e ? "," : "", e, sites[e].name, p->id, p->attach_btf_obj_id, p->attach_btf_id,
               e == 0 ? "virtio_gpu" : s->drm_owner,
               (uint64_t)p->run_cnt, (uint64_t)p->recursion_misses, (uint64_t)p->run_time_ns);
        if (after) {
            struct tally *t = &s->totals[e];
            append(s, ",\"calls\":%" PRIu64 ",\"disabled\":%" PRIu64 ",\"emitted\":%" PRIu64
                   ",\"reserve_failed\":%" PRIu64 ",\"read_failed\":%" PRIu64 ",\"retained_records\":%" PRIu64,
                   t->calls, t->disabled, t->emitted, t->reserve_failed, t->read_failed, s->retained[e]);
        }
        append(s, "}");
    }
    append(s, "]");
}
static void append_errors(struct state *s, uint64_t bits, const char *const *names, int count)
{
    append(s, "[");
    bool comma = false;
    for (int i = 0; i < count; i++)
        if (bits & BIT(i)) {
            append(s, "%s\"%s\"", comma ? "," : "", names[i]);
            comma = true;
        }
    append(s, "]");
}
/* O_EXCL temporary inode + no-replace link makes the final name appear only when complete. */
static bool publish(struct state *s, const char *temporary, const char *final)
{
    if (s->receipt_used >= sizeof(s->receipt))
        return false;
    int fd = openat(s->root, temporary, O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC, 0600);
    if (fd < 0)
        return false;
    bool ok = true, linked = false;
    size_t written = 0;
    while (written < s->receipt_used) {
        ssize_t n = write(fd, s->receipt + written, s->receipt_used - written);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0) { ok = false; break; }
        written += (size_t)n;
    }
    if (fsync(fd))
        ok = false;
    if (close(fd)) {
        cleanup_fail(s, C_CLOSE);
        ok = false;
    }
    if (ok) {
        if (linkat(s->root, temporary, s->root, final, 0))
            ok = false;
        else
            linked = true;
    }
    if (unlinkat(s->root, temporary, 0)) {
        cleanup_fail(s, C_RECEIPT);
        ok = false;
    }
    if (fsync(s->root)) {
        cleanup_fail(s, C_DIRECTORY);
        ok = false;
    }
    if (!ok && linked) {
        /* Never leave a final name whose publishing/cleanup operation failed. */
        if (unlinkat(s->root, final, 0) && errno != ENOENT)
            cleanup_fail(s, C_RECEIPT);
    }
    return ok;
}
static void append_json_bytes(struct state *s, const char *text, size_t length)
{
    append(s, "\"");
    for (size_t i = 0; i < length; i++) {
        unsigned char c = (unsigned char)text[i];
        if (c == '"' || c == '\\')
            append(s, "\\%c", c);
        else if (c < 0x20 || c >= 0x7f)
            append(s, "\\u%04x", (unsigned int)c);
        else
            append(s, "%c", c);
    }
    append(s, "\"");
}
static void append_quiescence(struct state *s)
{
    static const char *const stages[] = {"before", "active", "after"};
    append(s, ",\"quiescence_backend\":\"tracefs-function-ops-tasks-rcu-v1\","
           "\"quiescence_begin_monotonic_ns\":%" PRIu64 ","
           "\"quiescence_end_monotonic_ns\":%" PRIu64 ","
           "\"quiescence_completed\":%s", s->quiescence_begin, s->quiescence_end,
           s->quiescence_completed ? "true" : "false");
    for (int i = 0; i < 3; i++) {
        const struct registration *r = &s->registration[i];
        append(s, ",\"quiescence_registration_%s\":", stages[i]);
        if (!r->readable) {
            append(s, "null");
            continue;
        }
        append(s, "{\"count\":%u,\"raw_line\":", r->count);
        if (r->found)
            append_json_bytes(s, r->line, r->length);
        else
            append(s, "null");
        append(s, "}");
    }
}
static void append_cpu_inventory(struct state *s)
{
    append(s, ",\"possible_cpu_count\":%d,\"online_cpu_ids\":[", s->possible_cpus);
    for (int c = 0; c < s->cpus; c++)
        append(s, "%s%u", c ? "," : "", s->online_ids[c]);
    append(s, "]");
}
static void append_buffer_accounting(struct state *s)
{
    append(s, ",\"buffer_accounting\":{\"ring_data_bytes\":%u,"
           "\"native_event_ring_bytes\":0,\"per_event_heap_allocation\":false,"
           "\"per_event_userspace_payload_copy\":false,\"ring_data_virtual_aliases\":2,"
           "\"ring_data_virtual_bytes\":%u,\"ring_aliases_share_physical_pages\":true,"
           "\"ring_metadata_virtual_bytes\":%zu,\"native_static_state_bytes\":%zu,"
           "\"kernel_counter_lookup_scratch_bytes\":%zu,"
           "\"cpu_inventory_transient_read_limit_bytes\":%u,"
           "\"barrier_conservative_ring_reader_pages_per_cpu\":6,"
           "\"barrier_buffer_size_kb\":%lu,"
           "\"libbpf_reader_metadata_bytes_measured\":false,"
           "\"libbpf_loader_btf_memory_measured\":false}",
           RING_BYTES, 2 * RING_BYTES, 2 * s->page, sizeof(*s),
           s->counter_scratch_bytes, CPU_LIST_LIMIT, s->barrier_buffer_kb);
}
static bool publish_ready(struct state *s)
{
    s->receipt_used = 0;
    uint64_t ready = now_ns(s);
    append(s, "{\"schema\":1,\"ready_monotonic_ns\":%" PRIu64
           ",\"capture_enable_begin_monotonic_ns\":%" PRIu64 ",\"capture_started_monotonic_ns\":%" PRIu64
           ",\"programs_before\":", ready, s->enable_begin, s->started);
    append_programs(s, false);
    append_cpu_inventory(s);
    append_quiescence(s);
    append_buffer_accounting(s);
    append(s, "}\n");
    return publish(s, "bpf-ready.json.tmp", "bpf-ready.json");
}
static bool publish_result(struct state *s)
{
    s->receipt_used = 0;
    append(s, "{\"schema\":1,\"capture_enable_begin_monotonic_ns\":%" PRIu64
           ",\"capture_started_monotonic_ns\":%" PRIu64 ",\"capture_stop_begin_monotonic_ns\":%" PRIu64
           ",\"capture_stopped_monotonic_ns\":%" PRIu64 ",\"detached_monotonic_ns\":%" PRIu64
           ",\"programs_before\":", s->enable_begin, s->started, s->stop_begin, s->stopped, s->detached);
    append_programs(s, false);
    append(s, ",\"programs_after\":");
    append_programs(s, true);
    append(s, ",\"qualified_acquisition\":%s,\"errors\":", !s->errors && !s->cleanup_errors ? "true" : "false");
    append_errors(s, s->errors, error_names, ERROR_COUNT);
    append(s, ",\"cleanup_errors\":");
    append_errors(s, s->cleanup_errors, cleanup_names, CLEANUP_COUNT);
    append_cpu_inventory(s);
    append_quiescence(s);
    append_buffer_accounting(s);
    append(s, ",\"raw\":{\"bytes\":%" PRIu64 "}}\n", s->raw_bytes);
    return publish(s, "bpf-result.json.tmp", "bpf-result.json");
}

static void reconcile(struct state *s)
{
    uint64_t total_records = 0;
    for (int e = 0; e < EVENTS; e++) {
        if (!s->programs[e] || bpf_program__fd(s->programs[e]) < 0)
            continue;
        s->after_valid[e] = program_info(s, e, &s->after[e]);
        uint32_t key = (uint32_t)e;
        if (s->tally_fd < 0 || !s->counter_scratch ||
            bpf_map_lookup_elem(s->tally_fd, &key, s->counter_scratch)) {
            fail(s, E_TALLY);
            continue;
        }
        struct tally *t = &s->totals[e];
        for (int cpu = 0; cpu < s->possible_cpus; cpu++) {
            const struct tally *p = &s->counter_scratch[cpu];
            t->calls += p->calls;
            t->disabled += p->disabled;
            t->emitted += p->emitted;
            t->reserve_failed += p->reserve_failed;
            t->read_failed += p->read_failed;
            int slot = -1;
            for (int c = 0; c < s->cpus; c++)
                if (s->online_possible_slots[c] == (uint32_t)cpu) { slot = c; break; }
            if (slot < 0) {
                if (p->calls || p->emitted)
                    fail(s, E_CPU);
            } else {
                if (p->emitted != s->retained_cpu[e][slot])
                    fail(s, E_EMITTED);
                if (s->last_call[e][slot] > p->calls)
                    fail(s, E_SEQUENCE);
            }
        }
        if (s->before_valid[e] && s->after_valid[e]) {
            const struct bpf_prog_info *a = &s->after[e], *b = &s->before[e];
            if (a->id != b->id || a->attach_btf_obj_id != b->attach_btf_obj_id || a->attach_btf_id != b->attach_btf_id)
                fail(s, E_OWNER);
            if (a->recursion_misses || b->recursion_misses)
                fail(s, E_MISSES);
            if (a->run_cnt < b->run_cnt || a->run_time_ns < b->run_time_ns)
                fail(s, E_RUN_MONOTONIC);
            if (a->run_cnt != t->calls)
                fail(s, E_RUN_CALLS);
        }
        if (t->calls != t->disabled + t->emitted + t->reserve_failed)
            fail(s, E_CALL_BALANCE);
        if (t->reserve_failed)
            fail(s, E_RESERVE);
        if (t->read_failed)
            fail(s, E_READ);
        if (t->read_failed != s->read_failures[e])
            fail(s, E_READ_BALANCE);
        if (t->emitted != s->retained[e])
            fail(s, E_EMITTED);
        total_records += s->retained[e];
    }
    if (s->capture_ready && !total_records)
        fail(s, E_EMPTY);
    if (total_records * sizeof(struct record) != s->raw_bytes)
        fail(s, E_RECORD);
}
static void release_verifier_btf(struct state *s)
{
    for (int i = 0; i < 3; i++) {
        btf__free(s->owners[i].btf);
        s->owners[i].btf = NULL;
    }
    btf__free(s->base);
    s->base = NULL;
}
static bool read_barrier_file(struct state *s, const char *name, char *text, size_t capacity)
{
    int fd = openat(s->barrier, name, O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
    if (fd < 0)
        return false;
    size_t used = 0;
    bool ok = true;
    while (used < capacity) {
        ssize_t got = read(fd, text + used, capacity - used);
        if (got < 0 && errno == EINTR)
            continue;
        if (got < 0) { ok = false; break; }
        if (!got)
            break;
        used += (size_t)got;
    }
    close_checked(s, &fd);
    if (!ok || !used || used == capacity)
        return false;
    text[used] = '\0';
    return true;
}
static bool barrier_value(struct state *s, const char *name, const char *expected)
{
    char text[256];
    return read_barrier_file(s, name, text, sizeof(text) - 1) && !strcmp(text, expected);
}
static bool write_barrier_tracer(struct state *s, const char *value)
{
    int fd = openat(s->barrier, "current_tracer", O_WRONLY|O_NOFOLLOW|O_CLOEXEC);
    if (fd < 0)
        return false;
    size_t length = strlen(value);
    ssize_t written;
    do { written = write(fd, value, length); } while (written < 0 && errno == EINTR && in_time(s));
    bool ok = written == (ssize_t)length;
    close_checked(s, &fd);
    return ok;
}
static bool prepare_barrier(struct state *s, const char *path)
{
    static const char *const roots[] = {
        "/sys/kernel/tracing/instances/", "/sys/kernel/debug/tracing/instances/"
    };
    const char *name = NULL;
    const char *root = NULL;
    for (size_t i = 0; i < sizeof(roots)/sizeof(roots[0]); i++)
        if (!strncmp(path, roots[i], strlen(roots[i]))) {
            root = roots[i];
            name = path + strlen(root);
            break;
        }
    if (!name || !*name || strlen(name) > NAME_MAX)
        return false;
    for (const char *p = name; *p; p++)
        if (!((*p >= 'a' && *p <= 'z') || (*p >= 'A' && *p <= 'Z') ||
              (*p >= '0' && *p <= '9') || *p == '_' || *p == '-'))
            return false;
    int instances = open(root, O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
    if (instances < 0)
        return false;
    s->barrier = openat(instances, name, O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
    s->trace_root = openat(instances, "..", O_RDONLY|O_DIRECTORY|O_NOFOLLOW|O_CLOEXEC);
    close_checked(s, &instances);
    struct stat info;
    struct statfs filesystem;
    if (s->barrier < 0 || s->trace_root < 0 || fstat(s->barrier, &info) ||
        !S_ISDIR(info.st_mode) || info.st_uid != 0 || (info.st_mode & 0022) ||
        fstatfs(s->barrier, &filesystem) || filesystem.f_type != TRACEFS_MAGIC)
        return false;
    char filter[256], size[32];
    if (!barrier_value(s, "current_tracer", "nop\n") ||
        !barrier_value(s, "tracing_on", "0\n") ||
        !barrier_value(s, "events/enable", "0\n") ||
        !read_barrier_file(s, "set_ftrace_filter", filter, sizeof(filter) - 1) ||
        !read_barrier_file(s, "buffer_size_kb", size, sizeof(size) - 1))
        return false;
    size_t length = strlen(barrier_function);
    if (strncmp(filter, barrier_function, length))
        return false;
    const char *suffix = filter + length;
    if (strcmp(suffix, "\n") &&
        (strcmp(s->drm_owner, "drm") || strcmp(suffix, " [drm]\n")))
        return false;
    if (size[0] < '0' || size[0] > '9')
        return false;
    errno = 0;
    char *end;
    unsigned long kb = strtoul(size, &end, 10);
    if (errno || strcmp(end, "\n") || !kb || kb > 2 * s->page / 1024)
        return false;
    s->barrier_buffer_kb = kb;
    s->barrier_valid = true;
    return true;
}
static bool registration_line(struct state *s, struct registration *r, const char *line, size_t length)
{
    size_t prefix = strlen(barrier_function);
    if (length <= prefix || strncmp(line, barrier_function, prefix) || line[prefix] != ' ')
        return true;
    if (r->found)
        return false;
    const char *p = line + prefix;
    while (*p == ' ')
        p++;
    if (*p == '[') {
        if (strcmp(s->drm_owner, "drm") || strncmp(p, "[drm]", 5))
            return false;
        p += 5;
        while (*p == ' ')
            p++;
    }
    if (*p++ != '(' || *p < '0' || *p > '9')
        return false;
    uint32_t count = 0;
    do {
        unsigned int digit = (unsigned int)(*p++ - '0');
        if (count > (UINT32_MAX - digit) / 10)
            return false;
        count = count * 10 + digit;
    } while (*p >= '0' && *p <= '9');
    if (*p != ')' || !count)
        return false;
    memcpy(r->line, line, length);
    r->line[length] = '\0';
    r->length = length;
    r->count = count;
    r->found = true;
    return true;
}
static bool read_registration(struct state *s, struct registration *r)
{
    int fd = openat(s->trace_root, "enabled_functions", O_RDONLY|O_NOFOLLOW|O_CLOEXEC);
    if (fd < 0)
        return false;
    char buffer[4096], line[BARRIER_LINE_BYTES];
    size_t total = 0, used = 0;
    bool ok = true;
    for (;;) {
        if (!in_time(s)) { ok = false; break; }
        ssize_t got = read(fd, buffer, sizeof(buffer));
        if (got < 0 && errno == EINTR)
            continue;
        if (got < 0) { ok = false; break; }
        if (!got)
            break;
        total += (size_t)got;
        if (total > ENABLED_FUNCTIONS_LIMIT) { ok = false; break; }
        for (ssize_t i = 0; i < got; i++) {
            if (used == sizeof(line) - 1) { ok = false; break; }
            line[used++] = buffer[i];
            if (buffer[i] == '\n') {
                line[used] = '\0';
                if (!registration_line(s, r, line, used)) { ok = false; break; }
                used = 0;
            }
        }
        if (!ok)
            break;
    }
    close_checked(s, &fd);
    r->readable = ok && !used;
    return r->readable;
}
static void quiesce_programs(struct state *s)
{
    s->quiescence_begin = now_ns(s);
    if (!s->barrier_valid || !in_time(s) ||
        !barrier_value(s, "current_tracer", "nop\n") ||
        !barrier_value(s, "tracing_on", "0\n") ||
        !read_registration(s, &s->registration[0])) {
        fail(s, E_BARRIER);
        s->quiescence_end = now_ns(s);
        return;
    }
    bool active = write_barrier_tracer(s, "function\n") &&
        barrier_value(s, "current_tracer", "function\n") &&
        read_registration(s, &s->registration[1]);
    /* Always attempt teardown after the function write, even when evidence failed. */
    bool reset = write_barrier_tracer(s, "nop\n") &&
        barrier_value(s, "current_tracer", "nop\n");
    if (!reset)
        cleanup_fail(s, C_BARRIER_RESET);
    bool after = read_registration(s, &s->registration[2]);
    struct registration *r = s->registration;
    s->quiescence_end = now_ns(s);
    s->quiescence_completed = active && reset && after &&
        r[0].count != UINT32_MAX && r[1].count == r[0].count + 1 &&
        r[2].count == r[0].count && barrier_value(s, "tracing_on", "0\n") && in_time(s);
    if (!s->quiescence_completed)
        fail(s, E_BARRIER);
}
/*
 * Synchronous barrier, pinned source semantics:
 * linux v7.2.9 kernel/trace/trace_functions.c function_trace_reset() unregisters
 * the instance's kzalloc-owned ops; kernel/trace/ftrace.c marks those DYNAMIC
 * and ftrace_shutdown() waits synchronize_rcu_tasks_rude()+synchronize_rcu_tasks().
 * Successful tracer writes alone are insufficient (registration errors are
 * swallowed there), hence the retained before/+1/after enabled_functions proof.
 *
 * kernel/bpf/trampoline.c bpf_tramp_image_put() synchronously patches
 * ip_after_call -> ip_epilogue before scheduling deferred Tasks-RCU retirement.
 * arch/x86/net/bpf_jit_comp.c places that site before every fexit and the
 * epilogue after them. A target sleeping in its original body cannot execute
 * an old fexit on return. The explicit barrier waits remaining nonsleepable
 * programs/prologues; asynchronous original-body/image retirement may remain.
 * https://github.com/gregkh/linux/blob/v7.2.9/kernel/bpf/trampoline.c#L582-L628
 * https://github.com/gregkh/linux/blob/v7.2.9/kernel/trace/ftrace.c#L3252-L3270
 */
static void finish(struct state *s)
{
    uint32_t key = 0, off = 0;
    s->stop_begin = now_ns(s);
    if (s->enabled && bpf_map_update_elem(s->control, &key, &off, BPF_ANY)) {
        fail(s, E_CONTROL);
        cleanup_fail(s, C_DISABLE);
    }
    s->enabled = false;
    s->stopped = now_ns(s);
    bool had_links = false;
    for (int e = 0; e < EVENTS; e++) {
        if (s->links[e]) {
            had_links = true;
            if (bpf_link__destroy(s->links[e]))
                cleanup_fail(s, C_DETACH);
        }
        s->links[e] = NULL;
    }
    if (had_links)
        quiesce_programs(s);
    if (s->ring) {
        /* After physical detach there is at most one ring's worth left. Each pass
         * consumes at most256 records, including malformed zero-length records. */
        unsigned int passes = 0;
        int result;
        do {
            result = ring_buffer__consume(s->ring);
        } while (result == -EINTR && ++passes <= RING_BYTES / 8 / 256);
        if (result < 0)
            fail(s, E_RING);
    }
    s->detached = now_ns(s);
    if (s->max_record_time > s->detached)
        fail(s, E_TIMESTAMP);
    if (s->cpus && !online_cpus_unchanged(s))
        fail(s, E_CPU);
    if (s->loaded)
        reconcile(s);
    /* The stats reference remains live through physical detach and final program info. */
    close_checked(s, &s->stats);
    if (s->raw >= 0 && fsync(s->raw))
        cleanup_fail(s, C_RAW_SYNC);
    close_checked(s, &s->raw);
    /* libbpf's destructors are void: do not claim observed internal close/munmap
     * status. Qualification additionally requires this process to physically exit. */
    ring_buffer__free(s->ring);
    s->ring = NULL;
    release_verifier_btf(s);
    bpf_object__close(s->object);
    s->object = NULL;
    if (s->object_data && munmap(s->object_data, s->object_size))
        cleanup_fail(s, C_UNMAP);
    s->object_data = NULL;
    close_checked(s, &s->object_fd);
    close_checked(s, &s->barrier);
    close_checked(s, &s->trace_root);
    free(s->counter_scratch);
    s->counter_scratch = NULL;
}
static bool parse_number(const char *text, uint64_t *value)
{
    if (!text || !*text)
        return false;
    for (const char *p = text; *p; p++)
        if (*p < '0' || *p > '9')
            return false;
    errno = 0;
    char *end;
    unsigned long long n = strtoull(text, &end, 10);
    if (errno || *end || !n)
        return false;
    *value = (uint64_t)n;
    return true;
}
int main(int argc, char **argv)
{
    struct state *s = &state;
    s->root = s->raw = s->stats = s->control = s->tally_fd = s->ring_fd = s->object_fd = -1;
    s->barrier = s->trace_root = -1;
    const char *root = NULL, *object_path = "/usr/libexec/tuna-kernel-bpf.bpf.o";
    const char *barrier_path = NULL;
    bool deadline_set = false, owner_set = false, object_set = false, arguments_ok = true;
    uint64_t entered = now_ns(s);
    for (int i = 1; i < argc; i += 2) {
        if (i + 1 >= argc) { arguments_ok = false; break; }
        if (!strcmp(argv[i], "--root") && !root)
            root = argv[i + 1];
        else if (!strcmp(argv[i], "--deadline-ns") && !deadline_set) {
            deadline_set = true;
            arguments_ok &= parse_number(argv[i + 1], &s->deadline);
        } else if (!strcmp(argv[i], "--drm-owner") && !owner_set) {
            owner_set = true;
            s->drm_owner = argv[i + 1];
        } else if (!strcmp(argv[i], "--object") && !object_set) {
            object_set = true;
            object_path = argv[i + 1];
        } else if (!strcmp(argv[i], "--barrier-instance") && !barrier_path) {
            barrier_path = argv[i + 1];
        } else
            arguments_ok = false;
    }
    if (getuid() != 0 || geteuid() != 0 || !arguments_ok || !root || !barrier_path ||
        !deadline_set || !owner_set ||
        (strcmp(s->drm_owner, "vmlinux") && strcmp(s->drm_owner, "drm")) || object_path[0] != '/' ||
        !entered || s->deadline <= entered || s->deadline - entered > MAX_LIFETIME_NS) {
        fputs("tuna-kernel-bpf: invalid root-only invocation or deadline\n", stderr);
        return 2;
    }
    umask(0077);
    s->root = open_directory(s, root);
    if (s->root < 0) {
        fputs("tuna-kernel-bpf: output directory must be real root-owned mode0700\n", stderr);
        return 2;
    }
    struct sigaction action = {.sa_handler = stop_handler};
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGTERM, &action, NULL) || sigaction(SIGINT, &action, NULL)) {
        fail(s, E_SIGNALS);
        goto done;
    }
    s->raw = openat(s->root, "bpf.bin", O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW|O_CLOEXEC, 0600);
    if (s->raw < 0) { fail(s, E_RAW_OPEN); goto done; }
    long page = sysconf(_SC_PAGESIZE), online = sysconf(_SC_NPROCESSORS_ONLN);
    if (!prepare_cpus(s, online) ||
        page < 4096 || page > RING_BYTES || (page & (page - 1)) || RING_BYTES % page) {
        fail(s, E_CPU);
        goto done;
    }
    s->page = (size_t)page;
    if (!prepare_barrier(s, barrier_path)) { fail(s, E_BARRIER_CONFIG); goto done; }
    struct layout layout;
    if (!load_layout(s, &layout)) { fail(s, E_LAYOUT); goto done; }
    s->object_fd = open(object_path, O_RDONLY|O_NOFOLLOW|O_CLOEXEC|O_NONBLOCK);
    struct stat object_info;
    if (s->object_fd < 0 || fstat(s->object_fd, &object_info) || !S_ISREG(object_info.st_mode) ||
        object_info.st_uid != 0 || (object_info.st_mode & 0022) ||
        object_info.st_size < 1 || object_info.st_size > 1024 * 1024) {
        fail(s, E_OBJECT);
        goto done;
    }
    s->object_size = (size_t)object_info.st_size;
    s->object_data = mmap(NULL, s->object_size, PROT_READ, MAP_PRIVATE, s->object_fd, 0);
    if (s->object_data == MAP_FAILED) {
        s->object_data = NULL;
        fail(s, E_OBJECT);
        goto done;
    }
    s->object = bpf_object__open_mem(s->object_data, s->object_size, NULL);
    if (libbpf_get_error(s->object)) {
        s->object = NULL;
        fail(s, E_OBJECT);
        goto done;
    }
    int count = 0;
    struct bpf_program *p;
    bpf_object__for_each_program(p, s->object) {
        if (count >= EVENTS || strcmp(bpf_program__name(p), sites[count].symbol) ||
            strcmp(bpf_program__section_name(p), sites[count].section) ||
            bpf_program__type(p) != BPF_PROG_TYPE_TRACING ||
            bpf_program__expected_attach_type(p) != sites[count].attach_type) {
            fail(s, E_INVENTORY);
            goto done;
        }
        s->programs[count++] = p;
    }
    if (count != EVENTS) { fail(s, E_INVENTORY); goto done; }
    /*
     * The verified BTF offsets must be immutable BEFORE program verification.
     * verifier.c bpf_map_is_rdonly()/bpf_map_direct_read() then propagate their
     * actual values into typed-context pointer arithmetic. Freezing after
     * bpf_object__load is too late; arbitrary map-value offsets are not safe.
     * This is the same single offsets map, not an extra map or guessed layout.
     */
    struct bpf_map *layout_map = bpf_object__find_map_by_name(s->object, "offsets");
    if (!layout_map) { fail(s, E_MAPS); goto done; }
    LIBBPF_OPTS(bpf_map_create_opts, layout_options, .map_flags = BPF_F_RDONLY_PROG);
    int layout_fd = bpf_map_create(BPF_MAP_TYPE_ARRAY, "offsets", 4, sizeof(layout), 1, &layout_options);
    if (layout_fd < 0) { fail(s, E_LAYOUT_MAP_SETUP); goto done; }
    uint32_t layout_key = 0;
    int prepared = bpf_map_update_elem(layout_fd, &layout_key, &layout, BPF_ANY);
    if (!prepared)
        prepared = bpf_map_freeze(layout_fd);
    if (!prepared)
        prepared = bpf_map__reuse_fd(layout_map, layout_fd);
    close_checked(s, &layout_fd);
    if (prepared) { fail(s, E_LAYOUT_MAP_SETUP); goto done; }
    s->stats = bpf_enable_stats(BPF_STATS_RUN_TIME);
    if (s->stats < 0) { fail(s, E_STATS); goto done; }
    if (!in_time(s)) goto done;
    if (bpf_object__load(s->object)) { fail(s, E_LOAD); goto done; }
    s->loaded = true;
    s->control = bpf_object__find_map_fd_by_name(s->object, "enabled");
    s->tally_fd = bpf_object__find_map_fd_by_name(s->object, "tallies");
    s->ring_fd = bpf_object__find_map_fd_by_name(s->object, "records");
    int offsets = bpf_object__find_map_fd_by_name(s->object, "offsets");
    struct bpf_map *map;
    count = 0;
    bpf_object__for_each_map(map, s->object) { count++; }
    if (count != 4 || !map_matches(s->control, BPF_MAP_TYPE_ARRAY, 4, 4, 1, 0) ||
        !map_matches(s->tally_fd, BPF_MAP_TYPE_PERCPU_ARRAY, 4, sizeof(struct tally), EVENTS, 0) ||
        !map_matches(offsets, BPF_MAP_TYPE_ARRAY, 4, sizeof(struct layout), 1, BPF_F_RDONLY_PROG) ||
        !map_matches(s->ring_fd, BPF_MAP_TYPE_RINGBUF, 0, 0, RING_BYTES, 0)) {
        fail(s, E_MAPS);
        goto done;
    }
    uint32_t key = 0, on = 1;
    s->ring = ring_buffer__new(s->ring_fd, ring_event, s, NULL);
    if (!s->ring) { fail(s, E_RING_MAP); goto done; }
    for (int e = 0; e < EVENTS; e++) {
        if (!in_time(s)) goto done;
        s->before_valid[e] = program_info(s, e, &s->before[e]);
        if (!s->before_valid[e]) goto done;
        if (!verify_owner(s, e, &s->before[e])) { fail(s, E_OWNER); goto done; }
        if (s->before[e].recursion_misses) { fail(s, E_MISSES); goto done; }
        if (s->before[e].run_cnt || s->before[e].run_time_ns) { fail(s, E_RUN_MONOTONIC); goto done; }
    }
    for (int e = 0; e < EVENTS; e++) {
        if (!in_time(s)) goto done;
        if (stop_signal) { fail(s, E_EARLY_STOP); goto done; }
        s->links[e] = bpf_program__attach(s->programs[e]);
        if (libbpf_get_error(s->links[e])) {
            s->links[e] = NULL;
            fail(s, E_ATTACH);
            goto done;
        }
        struct bpf_prog_info attached;
        if (!program_info(s, e, &attached)) goto done;
        if (attached.id != s->before[e].id ||
            attached.attach_btf_obj_id != s->before[e].attach_btf_obj_id ||
            attached.attach_btf_id != s->before[e].attach_btf_id ||
            !verify_owner(s, e, &attached)) {
            fail(s, E_OWNER);
            goto done;
        }
    }
    /* Final info must match these retained program IDs/BTF IDs; keep no duplicate
     * verifier BTF buffers in the active event reader. */
    release_verifier_btf(s);
    if (s->cleanup_errors || stop_signal || !in_time(s)) {
        if (stop_signal) fail(s, E_EARLY_STOP);
        goto done;
    }
    s->enable_begin = now_ns(s);
    if (bpf_map_update_elem(s->control, &key, &on, BPF_ANY)) { fail(s, E_CONTROL); goto done; }
    s->enabled = true;
    s->started = now_ns(s);
    if (!publish_ready(s)) { fail(s, E_READY); goto done; }
    s->capture_ready = true;
    while (!stop_signal && !s->errors && !s->cleanup_errors && in_time(s)) {
        uint64_t current = now_ns(s);
        if (!current || current >= s->deadline) { fail(s, E_DEADLINE); break; }
        uint64_t left = s->deadline - current;
        int timeout = left / 1000000 > 20 ? 20 : (int)(left / 1000000);
        int result = ring_buffer__poll(s->ring, timeout);
        if (result < 0 && result != -EINTR)
            fail(s, E_POLL);
    }
 done:
    finish(s);
    in_time(s);
    bool result_ok = publish_result(s);
    if (!result_ok)
        fail(s, E_RESULT);
    /* The caller must require exit0 as well as a qualifying receipt: final publication/root
     * close failures happen after serialization and cannot honestly be certified by that JSON. */
    close_checked(s, &s->root);
    in_time(s);
    bool qualified = !s->errors && !s->cleanup_errors;
    if (!qualified) {
        for (int e = 0; e < ERROR_COUNT; e++)
            if (s->errors & BIT(e)) fprintf(stderr, "tuna-kernel-bpf: %s\n", error_names[e]);
        for (int c = 0; c < CLEANUP_COUNT; c++)
            if (s->cleanup_errors & BIT(c)) fprintf(stderr, "tuna-kernel-bpf: cleanup: %s\n", cleanup_names[c]);
    }
    return qualified ? 0 : 1;
}
