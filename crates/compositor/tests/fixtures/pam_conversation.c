/* Controlled CI module: no system account/password or hardware dependency. */
#include <security/pam_modules.h>
#include <security/pam_ext.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>

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
PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags;
    const char *mode = argc ? argv[0] : "multi";
    if (strcmp(mode, "hang") == 0) for (;;) pause();
    if (strcmp(mode, "crash") == 0) abort();
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
    return argc && strcmp(argv[0], "accountdeny") == 0 ? PAM_PERM_DENIED : PAM_SUCCESS;
}
