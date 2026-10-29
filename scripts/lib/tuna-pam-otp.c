/*
 * tuna-pam-otp: a controlled second PAM step for scripts/tuna-lock-auth-proof.
 *
 * Stacked after pam_matrix in a pam_wrapper service, it makes the lock
 * screen's `tuna-lock` conversation multi-step the way a real OTP/2FA
 * module does: one PAM_TEXT_INFO, then one visible (PAM_PROMPT_ECHO_ON)
 * question whose answer must equal the `code=` module argument. GNOME
 * Shell 51 renders the info as a message and the question as a new,
 * non-secret prompt step with Back
 * (https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/51.0/js/gdm/authPrompt.js),
 * and GDM's worker relays both through its conversation
 * (https://gitlab.gnome.org/GNOME/gdm/-/blob/51.0/daemon/gdm-session-worker.c).
 *
 * No system account, password database or hardware is touched. Every
 * response is wiped before it is freed, like
 * crates/compositor/tests/fixtures/pam_conversation.c.
 *
 * Build: cc -shared -fPIC -o tuna-pam-otp.so tuna-pam-otp.c -lpam
 * Use:   auth required /path/tuna-pam-otp.so code=123456
 */
#define PAM_SM_AUTH
#include <security/pam_modules.h>
#include <security/pam_appl.h>
#include <stdlib.h>
#include <string.h>

#define OTP_INFO "A verification code was sent to your device"
#define OTP_QUESTION "Verification code:"

static void wipe_response(struct pam_response *responses, int count) {
    if (responses == NULL)
        return;
    for (int i = 0; i < count; ++i) {
        if (responses[i].resp != NULL) {
            volatile char *p = responses[i].resp;
            size_t len = strlen(responses[i].resp);
            for (size_t j = 0; j < len; ++j)
                p[j] = 0;
            free(responses[i].resp);
            responses[i].resp = NULL;
        }
    }
    free(responses);
}

/* One message per conversation call, as pam_info/pam_prompt do. On
 * success *answer owns the (possibly NULL) response; the caller wipes it. */
static int converse(pam_handle_t *pamh, int style, const char *text,
                    struct pam_response **answer) {
    const struct pam_conv *conv = NULL;
    struct pam_message message = { style, text };
    const struct pam_message *messages[1] = { &message };
    struct pam_response *responses = NULL;
    *answer = NULL;
    if (pam_get_item(pamh, PAM_CONV, (const void **)&conv) != PAM_SUCCESS ||
        conv == NULL || conv->conv == NULL)
        return PAM_CONV_ERR;
    int result = conv->conv(1, messages, &responses, conv->appdata_ptr);
    if (result != PAM_SUCCESS) {
        wipe_response(responses, 1);
        return PAM_CONV_ERR;
    }
    *answer = responses;
    return PAM_SUCCESS;
}

/* Length-independent comparison: no early exit on the first mismatch. */
static int same_code(const char *given, const char *expected) {
    size_t given_len = strlen(given), expected_len = strlen(expected);
    unsigned char diff = given_len != expected_len;
    for (size_t i = 0; i < given_len; ++i)
        diff |= (unsigned char)given[i] ^ (unsigned char)expected[i % (expected_len ? expected_len : 1)];
    return diff == 0;
}

PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags;
    const char *code = NULL;
    for (int i = 0; i < argc; ++i)
        if (strncmp(argv[i], "code=", 5) == 0)
            code = argv[i] + 5;
    if (code == NULL || *code == '\0')
        return PAM_SERVICE_ERR;

    struct pam_response *answer = NULL;
    if (converse(pamh, PAM_TEXT_INFO, OTP_INFO, &answer) != PAM_SUCCESS)
        return PAM_CONV_ERR;
    wipe_response(answer, 1);

    if (converse(pamh, PAM_PROMPT_ECHO_ON, OTP_QUESTION, &answer) != PAM_SUCCESS)
        return PAM_CONV_ERR;
    int result = PAM_AUTH_ERR;
    if (answer != NULL && answer[0].resp != NULL && same_code(answer[0].resp, code))
        result = PAM_SUCCESS;
    wipe_response(answer, 1);
    return result;
}

PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh; (void)flags; (void)argc; (void)argv;
    return PAM_SUCCESS;
}
