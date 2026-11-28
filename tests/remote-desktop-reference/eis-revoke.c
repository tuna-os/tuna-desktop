/* R-EIS-REVOKE: a real libei sender holding a delivered RemoteDesktop EIS
 * connection must hear the compositor disconnect it once the grant is
 * revoked (ADR 0011 L7, G9). Prints "libei devices resumed" once its
 * keyboard and pointer are live, then "libei disconnected" when
 * EI_EVENT_DISCONNECT arrives, and exits 0. It never disconnects itself:
 * exit 4 for a disconnect before the devices resumed, 5 when they never
 * resumed within 15 s, 6 when no disconnect came within 30 s of that. */
#include <libei.h>
#include <poll.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    struct ei *ei = ei_new_sender(NULL);
    if (!ei || ei_setup_backend_fd(ei, atoi(argv[1])) != 0) return 3;
    bool keyboard = false, pointer = false, resumed = false;
    uint64_t deadline = ei_now(ei) + 15000000;
    for (;;) {
        struct pollfd fd = {.fd = ei_get_fd(ei), .events = POLLIN};
        poll(&fd, 1, 100);
        ei_dispatch(ei);
        struct ei_event *event;
        while ((event = ei_get_event(ei))) {
            enum ei_event_type type = ei_event_get_type(event);
            if (type == EI_EVENT_DISCONNECT) {
                ei_event_unref(event);
                if (!resumed) return 4;
                puts("libei disconnected");
                fflush(stdout);
                ei_unref(ei);
                return 0;
            }
            if (type == EI_EVENT_SEAT_ADDED)
                ei_seat_bind_capabilities(ei_event_get_seat(event), EI_DEVICE_CAP_KEYBOARD,
                    EI_DEVICE_CAP_POINTER_ABSOLUTE, EI_DEVICE_CAP_BUTTON, NULL);
            if (type == EI_EVENT_DEVICE_RESUMED) {
                struct ei_device *device = ei_event_get_device(event);
                keyboard |= ei_device_has_capability(device, EI_DEVICE_CAP_KEYBOARD);
                pointer |= ei_device_has_capability(device, EI_DEVICE_CAP_POINTER_ABSOLUTE);
            }
            ei_event_unref(event);
        }
        if (!resumed && keyboard && pointer) {
            resumed = true;
            puts("libei devices resumed");
            fflush(stdout);
            deadline = ei_now(ei) + 30000000;
        }
        if (ei_now(ei) > deadline) return resumed ? 6 : 5;
    }
}
