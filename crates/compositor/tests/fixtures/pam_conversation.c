/* Controlled CI module: no system account/password or hardware dependency. */
#include <security/pam_modules.h>
#include <security/pam_ext.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <limits.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <errno.h>

static int ask(pam_handle_t *pamh, int count, struct pam_message *messages,
               const char *const *expected) {
    const struct pam_conv *conv = NULL;
    const struct pam_message *ptrs[4];
    struct pam_response *responses = NULL;
    if (count < 1 || count > 4 || pam_get_item(pamh, PAM_CONV, (const void **)&conv) != PAM_SUCCESS)
        return PAM_SYSTEM_ERR;
    for (int i = 0; i < count; ++i) ptrs[i] = &messages[i];
    int result = conv->conv(count, ptrs, &responses, conv->appdata_ptr);
    if (result != PAM_SUCCESS || responses == NULL) return PAM_AUTH_ERR;
    for (int i = 0; i < count; ++i) {
        if (expected[i] == NULL) {
            if (responses[i].resp != NULL) result = PAM_AUTH_ERR;
        } else if (responses[i].resp == NULL || strcmp(expected[i], responses[i].resp) != 0) {
            result = PAM_AUTH_ERR;
        }
        if (responses[i].resp != NULL) {
            volatile char *p = responses[i].resp;
            size_t len = strlen(responses[i].resp);
            for (size_t j = 0; j < len; ++j) p[j] = 0;
            free(responses[i].resp);
        }
    }
    free(responses);
    return result;
}
static void hung_cleanup(pam_handle_t *pamh, void *data, int status) {
    (void)pamh; (void)data; (void)status;
    for (;;) pause();
}
/* "fprint-plan" modes read their plan and account state from files the
 * runtime proof writes under TUNA_AUTH_OWNER_ROOT; the plan is consumed. */
static int plan_path(char *path, size_t size, const char *name) {
    const char *root = getenv("TUNA_AUTH_OWNER_ROOT");
    if (root == NULL || strlen(root) > PATH_MAX - 64) return -1;
    snprintf(path, size, "%s/%s", root, name);
    return 0;
}
/* pam_fprintd's do_verify, scripted (fprintd pam/pam_fprintd.c): up to
 * three scans, each announced by an info message; 'r' is a retry status
 * within the scan (its error message, no try spent), 'm' matches, 'n' is a
 * no-match error, 't' times out (PAM_AUTHINFO_UNAVAIL); three no-matches
 * are PAM_MAXTRIES. No reader, daemon or print is involved. */
static int fingerprint(pam_handle_t *pamh, const char *mode) {
    char plan[16] = "";
    const char *script = mode + strlen("fprint-");
    if (strncmp(script, "plan", 4) == 0) {
        char path[PATH_MAX];
        if (plan_path(path, sizeof(path), "fprint-plan") != 0) return PAM_SYSTEM_ERR;
        int fd = open(path, O_RDONLY);
        if (fd < 0) return PAM_AUTHINFO_UNAVAIL;
        ssize_t got = read(fd, plan, sizeof(plan) - 1);
        close(fd);
        unlink(path);
        if (got < 0) return PAM_SYSTEM_ERR;
        plan[got] = 0;
        script = plan;
    }
    const char *none[] = { NULL };
    const char *step = script;
    for (int tries = 0; tries < 3; ++tries) {
        struct pam_message place = { PAM_TEXT_INFO, "Place your finger on the fingerprint reader" };
        if (ask(pamh, 1, &place, none) != PAM_SUCCESS) return PAM_AUTHINFO_UNAVAIL;
        for (; *step == 'r'; ++step) {
            struct pam_message retry = { PAM_ERROR_MSG, "Place your finger on the reader again" };
            if (ask(pamh, 1, &retry, none) != PAM_SUCCESS) return PAM_AUTHINFO_UNAVAIL;
        }
        char scan = *step ? *step++ : 't';
        if (scan == 'm') return PAM_SUCCESS;
        if (scan == 't') {
            struct pam_message late = { PAM_TEXT_INFO, "Verification timed out" };
            ask(pamh, 1, &late, none);
            return PAM_AUTHINFO_UNAVAIL;
        }
        struct pam_message miss = { PAM_ERROR_MSG, "Failed to match fingerprint" };
        if (ask(pamh, 1, &miss, none) != PAM_SUCCESS) return PAM_AUTHINFO_UNAVAIL;
    }
    return PAM_MAXTRIES;
}
static int fingerprint_account(const char *mode) {
    if (strcmp(mode, "fprint-plan") == 0) {
        char path[PATH_MAX];
        if (plan_path(path, sizeof(path), "fprint-expired") != 0) return PAM_SYSTEM_ERR;
        return access(path, F_OK) == 0 ? PAM_ACCT_EXPIRED : PAM_SUCCESS;
    }
    size_t len = strlen(mode);
    return len > 8 && strcmp(mode + len - 8, "-expired") == 0 ? PAM_ACCT_EXPIRED : PAM_SUCCESS;
}
PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags;
    const char *mode = argc ? argv[0] : "multi";
    if (strcmp(mode, "hang") == 0) for (;;) pause();
    if (strncmp(mode, "fprint-", 7) == 0) return fingerprint(pamh, mode);
    if (strcmp(mode, "crash") == 0) abort();
    if (strcmp(mode, "runtime-delay") == 0) {
        const char *root = getenv("TUNA_AUTH_OWNER_ROOT");
        char path[PATH_MAX];
        if (root == NULL || strlen(root) > PATH_MAX - 64) return PAM_SYSTEM_ERR;
        char pending[PATH_MAX], pid[32];
        snprintf(pending, sizeof(pending), "%s/started-pending-%ld", root, (long)getpid());
        int fd = open(pending, O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (fd < 0) return PAM_SYSTEM_ERR;
        int count = snprintf(pid, sizeof(pid), "%ld", (long)getpid());
        ssize_t written = write(fd, pid, (size_t)count);
        int closed = close(fd);
        if (written != count || closed != 0) { unlink(pending); return PAM_SYSTEM_ERR; }
        unsigned attempt = 1;
        for (; attempt <= 4; ++attempt) {
            snprintf(path, sizeof(path), "%s/started-%u", root, attempt);
            /* Publish complete PID bytes without replacing another helper's marker. */
            if (link(pending, path) == 0) break;
            if (errno != EEXIST) { unlink(pending); return PAM_SYSTEM_ERR; }
        }
        unlink(pending);
        if (attempt > 4) return PAM_SYSTEM_ERR;
        struct pam_message secret = { PAM_PROMPT_ECHO_OFF, "Controlled password" };
        const char *one[] = { "controlled-only" };
        if (ask(pamh, 1, &secret, one) != PAM_SUCCESS) return PAM_AUTH_ERR;
        snprintf(path, sizeof(path), "%s/release-%u", root, attempt);
        /* The private helper budget and outer test watchdog bound this fixture. */
        while (access(path, F_OK) != 0) usleep(5000);
        snprintf(path, sizeof(path), "%s/done-%u", root, attempt);
        fd = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
        if (fd < 0) return PAM_SYSTEM_ERR;
        close(fd);
        return PAM_SUCCESS;
    }
    if (strncmp(mode, "single", 6) == 0 || strcmp(mode, "repeat") == 0) {
        struct pam_message secret = { PAM_PROMPT_ECHO_OFF, "Password" };
        const char *one[] = { "right-password" };
        if (ask(pamh, 1, &secret, one) != PAM_SUCCESS) return PAM_AUTH_ERR;
        if (strcmp(mode, "repeat") == 0) {
            /* A single-password adapter must refuse this DISTINCT secret. */
            struct pam_message pin = { PAM_PROMPT_ECHO_OFF, "Separate PIN" };
            const char *other[] = { "pin-secret" };
            return ask(pamh, 1, &pin, other);
        }
        return strcmp(mode, "single-deny") == 0 ? PAM_AUTH_ERR : PAM_SUCCESS;
    }
    struct pam_message messages[] = {
        { PAM_PROMPT_ECHO_ON, "Visible account challenge" },
        { PAM_PROMPT_ECHO_OFF, "First secret challenge" },
        { PAM_TEXT_INFO, "Information from controlled stack" },
        { PAM_ERROR_MSG, "Error notice from controlled stack" }
    };
    const char *expected[] = { "visible-answer", "first-secret", NULL, NULL };
    if (ask(pamh, 4, messages, expected) != PAM_SUCCESS) return PAM_AUTH_ERR;
    struct pam_message pin = { PAM_PROMPT_ECHO_OFF, "Separate PIN challenge" };
    const char *pin_expected[] = { "pin-secret" };
    if (ask(pamh, 1, &pin, pin_expected) != PAM_SUCCESS) return PAM_AUTH_ERR;
    if (strcmp(mode, "userchange") == 0)
        return pam_set_item(pamh, PAM_USER, "a-different-controlled-account");
    if (strcmp(mode, "teardown") == 0)
        return pam_set_data(pamh, "controlled-hang", NULL, hung_cleanup);
    if (strcmp(mode, "child") == 0) {
        pid_t child = fork();
        if (child < 0) return PAM_SYSTEM_ERR;
        if (child == 0) { alarm(3); for (;;) pause(); }
        char text[64];
        snprintf(text, sizeof(text), "Owned child PID %ld", (long)child);
        struct pam_message notice = { PAM_TEXT_INFO, text };
        const char *none[] = { NULL };
        return ask(pamh, 1, &notice, none);
    }
    return strcmp(mode, "deny") == 0 ? PAM_AUTH_ERR : PAM_SUCCESS;
}
PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh; (void)flags; (void)argc; (void)argv; return PAM_SUCCESS;
}
PAM_EXTERN int pam_sm_acct_mgmt(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh; (void)flags;
    if (argc && strncmp(argv[0], "fprint-", 7) == 0) return fingerprint_account(argv[0]);
    return argc && (strcmp(argv[0], "accountdeny") == 0 || strcmp(argv[0], "single-accountdeny") == 0) ? PAM_PERM_DENIED : PAM_SUCCESS;
}
