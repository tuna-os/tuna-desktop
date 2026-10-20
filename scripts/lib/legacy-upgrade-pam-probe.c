/* tuna-rename: keep-file */
/* Test-only process: enter a disposable root before loading native PAM.
 * Never linked into, installed by, or executed from a release package. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/capability.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/resource.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <limits.h>
#include <unistd.h>

/* Linux-PAM's public ABI, also usable on test hosts without development
 * headers. pam_start_confdir has FIVE arguments, confdir before pamh.
 * There is deliberately no pam_start or host-configuration fallback. */
typedef struct pam_handle pam_handle_t;
struct pam_message { int msg_style; const char *msg; };
struct pam_response { char *resp; int resp_retcode; };
struct pam_conv {
    int (*conv)(int, const struct pam_message **, struct pam_response **, void *);
    void *appdata_ptr;
};
static int no_credentials(int count, const struct pam_message **messages,
                          struct pam_response **responses, void *data) {
    (void)count; (void)messages; (void)responses; (void)data;
    return 19; /* PAM_CONV_ERR: this synthetic test never supplies a secret. */
}
static void fail(const char *what) {
    fprintf(stderr, "private PAM isolation failed: %s (errno=%d)\n", what, errno);
    exit(125);
}
int main(int argc, char **argv) {
    struct stat before, after, forbidden;
    struct rlimit limit;
    struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
    struct __user_cap_data_struct capabilities[2] = {{0}, {0}};
    if (argc != 4 || argv[1][0] != '/' || argv[2][0] != '/' || argv[3][0] != '/')
        fail("fixed absolute arguments");
    for (int fd = 0; fd <= 2; ++fd)
        if (fstat(fd, &before) || !S_ISFIFO(before.st_mode)) fail("stdio must be pipes");
    if (geteuid() != 0 || stat(argv[1], &before) || !S_ISDIR(before.st_mode))
        fail("private root identity");
    if (prctl(PR_SET_PDEATHSIG, SIGKILL) || getppid() == 1)
        fail("parent lifetime");
    if (chdir(argv[1]) || chroot(".") || chdir("/") || stat("/", &after))
        fail("chroot");
    if (before.st_dev != after.st_dev || before.st_ino != after.st_ino)
        fail("private root changed");
    /* Keep only the three pipes supplied by the harness; no outside directory
     * or library descriptor survives the boundary. Never mount host paths. */
    if (syscall(SYS_close_range, 3u, ~0u, 0u)) {
        if (errno != ENOSYS || getrlimit(RLIMIT_NOFILE, &limit) || limit.rlim_cur == RLIM_INFINITY)
            fail("close outside descriptors");
        for (rlim_t fd = 3; fd < limit.rlim_cur; ++fd) close((int)fd);
    }
    if (getrlimit(RLIMIT_NOFILE, &limit) || limit.rlim_cur == RLIM_INFINITY)
        fail("descriptor verification bound");
    for (rlim_t fd = 3; fd < limit.rlim_cur; ++fd) {
        errno = 0;
        if (fcntl((int)fd, F_GETFD) != -1 || errno != EBADF) fail("outside descriptor survived");
    }
    for (int cap = 0; cap <= CAP_LAST_CAP; ++cap)
        if (prctl(PR_CAPBSET_DROP, cap, 0, 0, 0)) fail("drop capability bounding set");
    if (syscall(SYS_capset, &header, capabilities) ||
        prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) ||
        syscall(SYS_capget, &header, capabilities)) fail("drop capabilities");
    for (int i = 0; i < 2; ++i)
        if (capabilities[i].effective || capabilities[i].permitted || capabilities[i].inheritable)
            fail("capability verification");
    if (prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1) fail("no_new_privs verification");
    if (clearenv() || setenv("LC_ALL", "C", 1)) fail("clean environment");
    /* The harness first creates this unpredictable sentinel OUTSIDE the root.
     * A successful lookup here would invalidate all subsequent PAM evidence. */
    errno = 0;
    int outside = open(argv[2], O_RDONLY | O_CLOEXEC);
    if (outside != -1 || errno != ENOENT) fail("host sentinel can be opened");
    char traversal[PATH_MAX];
    if (snprintf(traversal, sizeof(traversal), "../../%s", argv[2]) >= (int)sizeof(traversal))
        fail("sentinel path bound");
    errno = 0;
    outside = open(traversal, O_RDONLY | O_CLOEXEC);
    if (outside != -1 || errno != ENOENT) fail("host sentinel traversal can be opened");
    errno = 0;
    if (stat("/proc/self/root", &forbidden) == 0 || errno != ENOENT) fail("host proc visible");
    errno = 0;
    if (stat("/etc/shadow", &forbidden) == 0 || errno != ENOENT) fail("host accounts visible");
    printf("{\"event\":\"ready\",\"pid\":%ld,\"chroot_verified\":true,"
           "\"outside_fds_closed\":true,\"capabilities_zero\":true,"
           "\"host_sentinel_absent\":true,\"host_proc_absent\":true,"
           "\"host_shadow_absent\":true,\"traversal_sentinel_absent\":true,"
           "\"no_new_privs\":true,\"pam_loaded\":false}\n", (long)getpid());
    fflush(stdout);
    char command[16];
    if (!fgets(command, sizeof(command), stdin) || strcmp(command, "INITIALIZE\n"))
        fail("initialization barrier");
    void *library = dlopen(argv[3], RTLD_NOW | RTLD_LOCAL);
    if (!library) { fprintf(stderr, "native PAM dlopen: %s\n", dlerror()); return 126; }
    int (*start)(const char *, const char *, const struct pam_conv *, const char *, pam_handle_t **);
    int (*authenticate)(pam_handle_t *, int);
    int (*account)(pam_handle_t *, int);
    int (*end)(pam_handle_t *, int);
    *(void **)(&start) = dlsym(library, "pam_start_confdir");
    *(void **)(&authenticate) = dlsym(library, "pam_authenticate");
    *(void **)(&account) = dlsym(library, "pam_acct_mgmt");
    *(void **)(&end) = dlsym(library, "pam_end");
    if (!start || !authenticate || !account || !end) return 127;
    struct pam_conv conversation = {no_credentials, NULL};
    pam_handle_t *handle = NULL;
    int started = start("roost-lock", "tuna-synthetic-test-user", &conversation,
                        "/etc/pam.d", &handle);
    int authenticated = -1, accounted = -1, ended = -1;
    if (started == 0) {
        authenticated = authenticate(handle, 0);
        accounted = account(handle, 0);
        ended = end(handle, authenticated);
    }
    printf("{\"event\":\"pam-result\",\"pid\":%ld,\"service\":\"roost-lock\","
           "\"confdir\":\"/etc/pam.d\",\"start\":%d,\"authenticate\":%d,"
           "\"account\":%d,\"end\":%d}\n",
           (long)getpid(), started, authenticated, accounted, ended);
    fflush(stdout);
    dlclose(library);
    return 0; /* The Python assertions decide whether these native results pass. */
}
