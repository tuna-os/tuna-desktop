/*
 * tuna-pam-fprint: a pam_fprintd stand-in for scripts/tuna-lock-auth-proof.
 *
 * The lock screen's fingerprint unlock is a PAM conversation on the
 * `tuna-lock-fingerprint` service (#1162 F5), whose packaged stack is
 * `auth required pam_fprintd.so` plus login's account stack. pam_fprintd
 * is not installed on CI, so this module runs pam_fprintd's do_auth /
 * do_verify loop the same way against fprintd's D-Bus API on
 * $DBUS_SYSTEM_BUS_ADDRESS (the proof's private bus with
 * scripts/lib/tuna-fprintd-mock.py), with pam_fprintd's defaults and
 * English messages
 * (https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/pam/pam_fprintd.c,
 * https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/pam/fingerprint-strings.h):
 *
 *   - no reader or no enrolled print: PAM_AUTHINFO_UNAVAIL;
 *   - Claim(user); up to three scans, each VerifyStart("any"), the
 *     "Place your finger on the fingerprint reader" (or swipe) info on
 *     VerifyFingerSelected, then the terminal VerifyStatus;
 *   - a match returns PAM_SUCCESS and simply disconnects (fprintd drops
 *     the claim of the vanished client);
 *   - a no-match reports "Failed to match fingerprint", then VerifyStop;
 *   - a 30 s silent scan reports "Verification timed out" and returns
 *     PAM_AUTHINFO_UNAVAIL; three no-matches return PAM_MAXTRIES; both
 *     VerifyStop and Release.
 * Non-terminal retry statuses are not exercised by the proof and are
 * ignored here.
 *
 * Its account hook stands in for an expired account: with
 * `expired=PATH` it returns PAM_ACCT_EXPIRED while PATH exists.
 *
 * Build: cc -shared -fPIC -o tuna-pam-fprint.so tuna-pam-fprint.c \
 *            $(pkg-config --cflags --libs gio-2.0) -lpam
 * Use:   auth    required /path/tuna-pam-fprint.so
 *        account required /path/tuna-pam-fprint.so expired=/path/flag
 */
#define PAM_SM_AUTH
#define PAM_SM_ACCOUNT
#include <security/pam_modules.h>
#include <security/pam_appl.h>
#include <gio/gio.h>
#include <string.h>
#include <unistd.h>

#define FPRINT_NAME "net.reactivated.Fprint"
#define DEVICE_IFACE "net.reactivated.Fprint.Device"
#define MAX_TRIES 3
#define TIMEOUT_SECONDS 30
#define CALL_TIMEOUT_MS 10000

static int send_msg(pam_handle_t *pamh, int style, const char *text) {
    const struct pam_conv *conv = NULL;
    struct pam_message message = { style, text };
    const struct pam_message *messages[1] = { &message };
    struct pam_response *responses = NULL;
    if (pam_get_item(pamh, PAM_CONV, (const void **)&conv) != PAM_SUCCESS ||
        conv == NULL || conv->conv == NULL)
        return PAM_CONV_ERR;
    int result = conv->conv(1, messages, &responses, conv->appdata_ptr);
    if (responses != NULL) {
        free(responses[0].resp);
        free(responses);
    }
    return result;
}

typedef struct {
    gboolean selected;
    gboolean done;
    gboolean timed_out;
    char *result;
} Scan;

static void on_signal(GDBusConnection *conn, const gchar *sender, const gchar *path,
                      const gchar *iface, const gchar *member, GVariant *params,
                      gpointer data) {
    (void)conn; (void)sender; (void)path; (void)iface;
    Scan *scan = data;
    if (strcmp(member, "VerifyFingerSelected") == 0) {
        scan->selected = TRUE;
    } else if (strcmp(member, "VerifyStatus") == 0 &&
               g_variant_is_of_type(params, G_VARIANT_TYPE("(sb)"))) {
        const gchar *result = NULL;
        gboolean done = FALSE;
        g_variant_get(params, "(&sb)", &result, &done);
        if (done && !scan->done) {
            scan->result = g_strdup(result);
            scan->done = TRUE;
        }
    }
}

static gboolean on_timeout(gpointer data) {
    ((Scan *)data)->timed_out = TRUE;
    return G_SOURCE_REMOVE;
}

static GVariant *call(GDBusConnection *conn, const char *path, const char *iface,
                      const char *method, GVariant *args, const char *reply) {
    return g_dbus_connection_call_sync(conn, FPRINT_NAME, path, iface, method, args,
                                       reply ? G_VARIANT_TYPE(reply) : NULL,
                                       G_DBUS_CALL_FLAGS_NONE, CALL_TIMEOUT_MS, NULL, NULL);
}

static gboolean device_call(GDBusConnection *conn, const char *path, const char *method,
                            GVariant *args) {
    GVariant *reply = call(conn, path, DEVICE_IFACE, method, args, NULL);
    if (reply == NULL)
        return FALSE;
    g_variant_unref(reply);
    return TRUE;
}

static gboolean is_swipe(GDBusConnection *conn, const char *path) {
    GVariant *reply = call(conn, path, "org.freedesktop.DBus.Properties", "Get",
                           g_variant_new("(ss)", DEVICE_IFACE, "scan-type"), "(v)");
    gboolean swipe = FALSE;
    if (reply != NULL) {
        GVariant *value = NULL;
        g_variant_get(reply, "(v)", &value);
        swipe = g_variant_is_of_type(value, G_VARIANT_TYPE_STRING) &&
                strcmp(g_variant_get_string(value, NULL), "swipe") == 0;
        g_variant_unref(value);
        g_variant_unref(reply);
    }
    return swipe;
}

/* pam_fprintd's do_verify for one claimed device. */
static int verify(pam_handle_t *pamh, GDBusConnection *conn, GMainContext *context,
                  const char *path, gboolean swipe) {
    const char *place = swipe ? "Swipe your finger across the fingerprint reader"
                              : "Place your finger on the fingerprint reader";
    for (int tries = 0; tries < MAX_TRIES; ++tries) {
        Scan scan = { FALSE, FALSE, FALSE, NULL };
        guint subscription = g_dbus_connection_signal_subscribe(
            conn, NULL, DEVICE_IFACE, NULL, path, NULL, G_DBUS_SIGNAL_FLAGS_NONE,
            on_signal, &scan, NULL);
        if (!device_call(conn, path, "VerifyStart", g_variant_new("(s)", "any"))) {
            g_dbus_connection_signal_unsubscribe(conn, subscription);
            return PAM_AUTHINFO_UNAVAIL;
        }
        GSource *timeout = g_timeout_source_new_seconds(TIMEOUT_SECONDS);
        g_source_set_callback(timeout, on_timeout, &scan, NULL);
        g_source_attach(timeout, context);
        gboolean announced = FALSE;
        while (!scan.done && !scan.timed_out) {
            g_main_context_iteration(context, TRUE);
            if (scan.selected && !announced) {
                announced = TRUE;
                send_msg(pamh, PAM_TEXT_INFO, place);
            }
        }
        g_source_destroy(timeout);
        g_source_unref(timeout);
        g_dbus_connection_signal_unsubscribe(conn, subscription);
        if (scan.timed_out && !scan.done) {
            send_msg(pamh, PAM_TEXT_INFO, "Verification timed out");
        } else if (strcmp(scan.result, "verify-match") == 0) {
            g_free(scan.result);
            /* Simply disconnect from the bus on PAM_SUCCESS. */
            return PAM_SUCCESS;
        } else if (strcmp(scan.result, "verify-no-match") == 0) {
            send_msg(pamh, PAM_ERROR_MSG, "Failed to match fingerprint");
        }
        device_call(conn, path, "VerifyStop", NULL);
        gboolean no_match = !scan.timed_out && strcmp(scan.result, "verify-no-match") == 0;
        gboolean unavailable = scan.timed_out ||
                               strcmp(scan.result, "verify-unknown-error") == 0 ||
                               strcmp(scan.result, "verify-disconnected") == 0;
        g_free(scan.result);
        if (unavailable)
            return PAM_AUTHINFO_UNAVAIL;
        if (!no_match) {
            send_msg(pamh, PAM_ERROR_MSG, "An unknown error occurred");
            return PAM_AUTH_ERR;
        }
    }
    return PAM_MAXTRIES;
}

PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)flags; (void)argc; (void)argv;
    const char *user = NULL;
    const char *address = getenv("DBUS_SYSTEM_BUS_ADDRESS");
    if (pam_get_user(pamh, &user, NULL) != PAM_SUCCESS || user == NULL || address == NULL)
        return PAM_AUTHINFO_UNAVAIL;
    GMainContext *context = g_main_context_new();
    g_main_context_push_thread_default(context);
    int status = PAM_AUTHINFO_UNAVAIL;
    GDBusConnection *conn = g_dbus_connection_new_for_address_sync(
        address,
        G_DBUS_CONNECTION_FLAGS_AUTHENTICATION_CLIENT |
            G_DBUS_CONNECTION_FLAGS_MESSAGE_BUS_CONNECTION,
        NULL, NULL, NULL);
    if (conn != NULL) {
        GVariant *device = call(conn, "/net/reactivated/Fprint/Manager",
                                "net.reactivated.Fprint.Manager", "GetDefaultDevice", NULL,
                                "(o)");
        if (device != NULL) {
            const char *path = NULL;
            g_variant_get(device, "(&o)", &path);
            GVariant *prints = call(conn, path, DEVICE_IFACE, "ListEnrolledFingers",
                                    g_variant_new("(s)", user), "(as)");
            gboolean enrolled = FALSE;
            if (prints != NULL) {
                GVariant *names = g_variant_get_child_value(prints, 0);
                enrolled = g_variant_n_children(names) > 0;
                g_variant_unref(names);
                g_variant_unref(prints);
            }
            if (enrolled &&
                device_call(conn, path, "Claim", g_variant_new("(s)", user))) {
                status = verify(pamh, conn, context, path, is_swipe(conn, path));
                if (status != PAM_SUCCESS)
                    device_call(conn, path, "Release", NULL);
            }
            g_variant_unref(device);
        }
        g_dbus_connection_close_sync(conn, NULL, NULL);
        g_object_unref(conn);
    }
    while (g_main_context_iteration(context, FALSE)) {
    }
    g_main_context_pop_thread_default(context);
    g_main_context_unref(context);
    return status;
}

PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh; (void)flags; (void)argc; (void)argv;
    return PAM_SUCCESS;
}

PAM_EXTERN int pam_sm_acct_mgmt(pam_handle_t *pamh, int flags, int argc, const char **argv) {
    (void)pamh; (void)flags;
    for (int i = 0; i < argc; ++i) {
        if (strncmp(argv[i], "expired=", 8) == 0 && access(argv[i] + 8, F_OK) == 0)
            return PAM_ACCT_EXPIRED;
    }
    return PAM_SUCCESS;
}
