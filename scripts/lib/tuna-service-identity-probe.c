/* LD_PRELOAD probe that makes one D-Bus call from inside an installed
 * service's genuine executable (ADR 0010 F1: same-UID code can run there).
 *
 * The compositor authenticates a capture caller by its executable
 * (/proc/PID/exe, pinned by pidfd) and by the well-known name it owns, so
 * this is exactly the identity it sees for the real service. Before the
 * service's main() runs, the constructor owns TUNA_PROBE_NAME (replacing
 * an owner that allows it, as the service's own --replace does), calls
 * TUNA_PROBE_METHOD ("interface.Method") on TUNA_PROBE_DEST at
 * TUNA_PROBE_PATH with the GVariant text TUNA_PROBE_ARGS, writes one line to
 * TUNA_PROBE_OUT and exits without running the service:
 *
 *   accepted <reply>
 *   error <D-Bus error name> <message>
 *
 * Exit status: 0 when the call was answered either way, 2 for a missing
 * variable, 3 when the name could not be owned, 4 for a bus failure.
 * Without TUNA_PROBE_OUT the library does nothing. */
#include <gio/gio.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *require(const char *variable) {
    const char *value = getenv(variable);
    if (!value || !*value) {
        fprintf(stderr, "tuna-service-identity-probe: %s is unset\n", variable);
        _exit(2);
    }
    return value;
}

__attribute__((constructor)) static void probe(void) {
    const char *out_path = getenv("TUNA_PROBE_OUT");
    if (!out_path) return;
    unsetenv("LD_PRELOAD");
    const char *name = require("TUNA_PROBE_NAME");
    const char *dest = require("TUNA_PROBE_DEST");
    const char *path = require("TUNA_PROBE_PATH");
    const char *method = require("TUNA_PROBE_METHOD");
    const char *args_text = require("TUNA_PROBE_ARGS");
    const char *dot = strrchr(method, '.');
    if (!dot) _exit(2);
    g_autofree char *interface = g_strndup(method, dot - method);
    GError *error = NULL;
    g_autoptr(GVariant) args = g_variant_parse(NULL, args_text, NULL, NULL, &error);
    if (!args || !g_variant_is_of_type(args, G_VARIANT_TYPE_TUPLE)) {
        fprintf(stderr, "tuna-service-identity-probe: TUNA_PROBE_ARGS is no tuple: %s\n",
                error ? error->message : args_text);
        _exit(2);
    }
    GDBusConnection *bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!bus) {
        fprintf(stderr, "tuna-service-identity-probe: %s\n", error->message);
        _exit(4);
    }
    /* DBUS_NAME_FLAG_REPLACE_EXISTING | DBUS_NAME_FLAG_DO_NOT_QUEUE; 1 is
     * DBUS_REQUEST_NAME_REPLY_PRIMARY_OWNER. */
    g_autoptr(GVariant) owned = g_dbus_connection_call_sync(bus, "org.freedesktop.DBus",
        "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        g_variant_new("(su)", name, 6), G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE,
        10000, NULL, &error);
    guint32 reply_code = 0;
    if (owned) g_variant_get(owned, "(u)", &reply_code);
    if (reply_code != 1) {
        fprintf(stderr, "tuna-service-identity-probe: could not own %s\n", name);
        _exit(3);
    }
    g_autoptr(GVariant) reply = g_dbus_connection_call_sync(bus, dest, path, interface,
        dot + 1, args, NULL, G_DBUS_CALL_FLAGS_NONE, 10000, NULL, &error);
    FILE *out = fopen(out_path, "w");
    if (!out) _exit(4);
    if (reply) {
        g_autofree char *text = g_variant_print(reply, FALSE);
        fprintf(out, "accepted %s\n", text);
    } else {
        g_autofree char *remote = g_dbus_error_get_remote_error(error);
        g_dbus_error_strip_remote_error(error);
        fprintf(out, "error %s %s\n", remote ? remote : "local", error->message);
    }
    fclose(out);
    _exit(0);
}
