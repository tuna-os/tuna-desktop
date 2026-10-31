/* The real libei proof sender must not revoke its grant before delivery. */
#include <fcntl.h>
#include <libeis.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

#define REQUIRE(condition) do { \
    if (!(condition)) { \
        fprintf(stderr, "EIS lifetime assertion failed at line %d: %s\n", __LINE__, #condition); \
        goto cleanup; \
    } \
} while (0)

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    /* Podman may launch the fixture with stdin closed. Reserve its standard
     * descriptor before creating protocol/ack sockets and dup2'ing the child. */
    int input = open("/dev/null", O_RDONLY | O_CLOEXEC);
    if (input < 0 || dup2(input, STDIN_FILENO) < 0) return 2;
    if (input != STDIN_FILENO) close(input);
    int result = 1, status = 0, acknowledge[2], receipt[2];
    pid_t child = -1;
    struct eis *eis = eis_new(NULL);
    struct eis_client *client = NULL;
    struct eis_device *device = NULL;
    REQUIRE(eis && eis_set_flag(eis, EIS_FLAG_DEVICE_READY) == 0 && eis_setup_backend_fd(eis) == 0);
    int client_fd = eis_backend_fd_add_client(eis);
    REQUIRE(client_fd >= 0 && pipe(acknowledge) == 0 && pipe(receipt) == 0);
    child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        char fd[32];
        snprintf(fd, sizeof fd, "%d", client_fd);
        if (fcntl(client_fd, F_SETFD, 0) != 0) _exit(126);
        dup2(acknowledge[0], STDIN_FILENO);
        dup2(receipt[1], STDOUT_FILENO);
        close(acknowledge[0]); close(acknowledge[1]);
        close(receipt[0]); close(receipt[1]);
        execl(argv[1], argv[1], fd, NULL);
        _exit(127);
    }
    close(client_fd);
    close(acknowledge[0]); close(receipt[1]);
    bool sent = false, acknowledged = false;
    unsigned keys = 0, buttons = 0, absolute = 0, frames = 0;
    uint64_t deadline = eis_now(eis) + 10000000;
    while (eis_now(eis) < deadline) {
        struct pollfd fds[] = {
            {.fd = eis_get_fd(eis), .events = POLLIN},
            {.fd = sent ? -1 : receipt[0], .events = POLLIN},
        };
        REQUIRE(poll(fds, 2, 50) >= 0);
        if (!sent && (fds[1].revents & (POLLIN | POLLHUP))) {
            char text[128] = {0};
            ssize_t size = read(receipt[0], text, sizeof text - 1);
            REQUIRE(size > 0 && strcmp(text, "libei keyboard and absolute pointer/button frames sent\n") == 0);
            sent = true;
            /* Deliberately outlast the obsolete 200 ms sender sleep. This is
             * a delayed real consumer, not a replacement input implementation. */
            usleep(350000);
            REQUIRE(waitpid(child, &status, WNOHANG) == 0);
        }
        eis_dispatch(eis);
        struct eis_event *event;
        while ((event = eis_get_event(eis))) {
            enum eis_event_type type = eis_event_get_type(event);
            if (type == EIS_EVENT_CLIENT_CONNECT) {
                client = eis_client_ref(eis_event_get_client(event));
                REQUIRE(eis_client_is_sender(client));
                eis_client_connect(client);
                struct eis_seat *seat = eis_client_new_seat(client, "lifetime regression");
                eis_seat_configure_capability(seat, EIS_DEVICE_CAP_KEYBOARD);
                eis_seat_configure_capability(seat, EIS_DEVICE_CAP_POINTER_ABSOLUTE);
                eis_seat_configure_capability(seat, EIS_DEVICE_CAP_BUTTON);
                eis_seat_add(seat);
                eis_seat_unref(seat);
            } else if (type == EIS_EVENT_SEAT_BIND
                       && eis_event_seat_has_capability(event, EIS_DEVICE_CAP_KEYBOARD)
                       && eis_event_seat_has_capability(event, EIS_DEVICE_CAP_POINTER_ABSOLUTE)
                       && eis_event_seat_has_capability(event, EIS_DEVICE_CAP_BUTTON)) {
                REQUIRE(!device);
                device = eis_seat_new_device(eis_event_get_seat(event));
                eis_device_configure_type(device, EIS_DEVICE_TYPE_VIRTUAL);
                eis_device_configure_name(device, "lifetime keyboard and pointer");
                eis_device_configure_capability(device, EIS_DEVICE_CAP_KEYBOARD);
                eis_device_configure_capability(device, EIS_DEVICE_CAP_POINTER_ABSOLUTE);
                eis_device_configure_capability(device, EIS_DEVICE_CAP_BUTTON);
                struct eis_region *region = eis_device_new_region(device);
                eis_region_set_size(region, 1280, 800);
                eis_region_add(region);
                eis_region_unref(region);
                eis_device_add(device);
            } else if (type == EIS_EVENT_DEVICE_READY) {
                eis_device_resume(eis_event_get_device(event));
            } else if (type == EIS_EVENT_KEYBOARD_KEY) {
                REQUIRE(eis_event_keyboard_get_key(event) == 30);
                REQUIRE(eis_event_keyboard_get_key_is_press(event) == (keys == 0));
                keys++;
            } else if (type == EIS_EVENT_BUTTON_BUTTON) {
                REQUIRE(eis_event_button_get_button(event) == 0x110);
                REQUIRE(eis_event_button_get_is_press(event) == (buttons == 0));
                buttons++;
            } else if (type == EIS_EVENT_POINTER_MOTION_ABSOLUTE) {
                REQUIRE(eis_event_pointer_get_absolute_x(event) == 640);
                REQUIRE(eis_event_pointer_get_absolute_y(event) == 400);
                absolute++;
            } else if (type == EIS_EVENT_FRAME) {
                frames++;
            } else if (type == EIS_EVENT_CLIENT_DISCONNECT) {
                REQUIRE(acknowledged);
                eis_event_unref(event);
                REQUIRE(waitpid(child, &status, 0) == child);
                child = -1;
                REQUIRE(WIFEXITED(status) && WEXITSTATUS(status) == 0);
                puts("libei lifetime: delayed real EIS delivery precedes acknowledged disconnect");
                result = 0;
                goto cleanup;
            }
            eis_event_unref(event);
        }
        if (sent && !acknowledged && keys == 2 && buttons == 2 && absolute == 1 && frames == 5) {
            REQUIRE(write(acknowledge[1], "\n", 1) == 1);
            acknowledged = true;
            deadline = eis_now(eis) + 2000000;
        }
    }
    fprintf(stderr, "EIS lifetime delivery/disconnect deadline expired: sent=%d acknowledged=%d keys=%u buttons=%u absolute=%u frames=%u\n",
            sent, acknowledged, keys, buttons, absolute, frames);
cleanup:
    if (child > 0) {
        kill(child, SIGKILL);
        waitpid(child, &status, 0);
    }
    if (device) eis_device_unref(device);
    if (client) eis_client_unref(client);
    if (eis) eis_unref(eis);
    return result;
}
