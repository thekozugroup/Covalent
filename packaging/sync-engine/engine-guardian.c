#define _POSIX_C_SOURCE 200809L

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#if defined(__linux__)
#include <sys/prctl.h>
#endif

enum {
    DEFAULT_GRACE_MS = 2000,
    MIN_GRACE_MS = 100,
    MAX_GRACE_MS = 10000,
    POLL_INTERVAL_MS = 250,
    MAX_INHERITED_FDS = 16384,
    GUARDIAN_FAILURE = 70,
    EXEC_FAILURE = 127,
};

static volatile sig_atomic_t stop_signal;
static volatile sig_atomic_t child_changed;

static void handle_signal(int signo)
{
    if (signo == SIGCHLD) {
        child_changed = 1;
    } else {
        stop_signal = signo;
    }
}

static int install_signal_handlers(void)
{
    struct sigaction action;
    struct sigaction ignore_pipe;

    memset(&action, 0, sizeof(action));
    action.sa_handler = handle_signal;
    sigemptyset(&action.sa_mask);
    action.sa_flags = SA_NOCLDSTOP;
    if (sigaction(SIGTERM, &action, NULL) < 0 ||
        sigaction(SIGINT, &action, NULL) < 0 ||
        sigaction(SIGHUP, &action, NULL) < 0 ||
        sigaction(SIGCHLD, &action, NULL) < 0) {
        return -1;
    }
    memset(&ignore_pipe, 0, sizeof(ignore_pipe));
    ignore_pipe.sa_handler = SIG_IGN;
    sigemptyset(&ignore_pipe.sa_mask);
    if (sigaction(SIGPIPE, &ignore_pipe, NULL) < 0) {
        return -1;
    }
    return 0;
}

static int set_cloexec(int fd)
{
    int flags = fcntl(fd, F_GETFD);
    if (flags < 0 || fcntl(fd, F_SETFD, flags | FD_CLOEXEC) < 0) {
        return -1;
    }
    return 0;
}

static int set_nonblocking(int fd)
{
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || fcntl(fd, F_SETFL, flags | O_NONBLOCK) < 0) {
        return -1;
    }
    return 0;
}

static int parse_descriptor_name(const char *name, int *result)
{
    unsigned long value = 0;
    const unsigned char *cursor = (const unsigned char *)name;

    if (*cursor == '\0') {
        return -1;
    }
    while (*cursor != '\0') {
        if (*cursor < '0' || *cursor > '9') {
            return -1;
        }
        value = value * 10 + (unsigned long)(*cursor - '0');
        if (value > INT_MAX) {
            return -1;
        }
        cursor++;
    }
    *result = (int)value;
    return 0;
}

static int collect_inherited_descriptors(int **result_fds, size_t *result_count)
{
#if defined(__APPLE__)
    const char *directory_path = "/dev/fd";
#elif defined(__linux__)
    const char *directory_path = "/proc/self/fd";
#else
#error "engine-guardian needs a reviewed descriptor inventory path"
#endif
    int *fds = calloc(MAX_INHERITED_FDS, sizeof(*fds));
    DIR *directory;
    struct dirent *entry;
    int inventory_fd;
    size_t count = 0;
    int saved_errno = 0;

    if (fds == NULL) {
        return -1;
    }
    directory = opendir(directory_path);
    if (directory == NULL) {
        free(fds);
        return -1;
    }
    inventory_fd = dirfd(directory);
    if (inventory_fd < 0) {
        saved_errno = errno;
        closedir(directory);
        free(fds);
        errno = saved_errno;
        return -1;
    }

    errno = 0;
    while ((entry = readdir(directory)) != NULL) {
        int fd;
        if (parse_descriptor_name(entry->d_name, &fd) < 0 || fd < 4 || fd == inventory_fd) {
            continue;
        }
        if (count == MAX_INHERITED_FDS) {
            saved_errno = EMFILE;
            break;
        }
        fds[count++] = fd;
    }
    if (saved_errno == 0 && errno != 0) {
        saved_errno = errno;
    }
    if (closedir(directory) < 0 && saved_errno == 0) {
        saved_errno = errno;
    }
    if (saved_errno != 0) {
        free(fds);
        errno = saved_errno;
        return -1;
    }
    *result_fds = fds;
    *result_count = count;
    return 0;
}

static void write_literal(const char *text)
{
    size_t remaining = strlen(text);
    const char *cursor = text;

    while (remaining != 0) {
        ssize_t written = write(STDERR_FILENO, cursor, remaining);
        if (written > 0) {
            cursor += written;
            remaining -= (size_t)written;
        } else if (written < 0 && errno == EINTR) {
            continue;
        } else {
            break;
        }
    }
}

static void report_generic(const char *text)
{
    write_literal("engine guardian: ");
    write_literal(text);
    write_literal("\n");
}

_Noreturn static void child_report_and_exit(int error_fd, int error_number)
{
    int saved_errno = error_number;
    ssize_t ignored;

    do {
        ignored = write(error_fd, &saved_errno, sizeof(saved_errno));
    } while (ignored < 0 && errno == EINTR);
    (void)ignored;
    _exit(EXEC_FAILURE);
}

static int parse_grace_ms(const char *text, int *result)
{
    long value = 0;
    const unsigned char *cursor = (const unsigned char *)text;

    if (*cursor == '\0') {
        return -1;
    }
    while (*cursor != '\0') {
        if (*cursor < '0' || *cursor > '9') {
            return -1;
        }
        value = value * 10 + (*cursor - '0');
        if (value > MAX_GRACE_MS) {
            return -1;
        }
        cursor++;
    }
    if (value < MIN_GRACE_MS) {
        return -1;
    }
    *result = (int)value;
    return 0;
}

static int64_t monotonic_ms(void)
{
    struct timespec now;

    if (clock_gettime(CLOCK_MONOTONIC, &now) < 0) {
        return -1;
    }
    return (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

static int wait_for_child(pid_t child, int *status, int nohang)
{
    pid_t result;

    do {
        result = waitpid(child, status, nohang ? WNOHANG : 0);
    } while (result < 0 && errno == EINTR);
    if (result == child) {
        return 1;
    }
    if (result == 0) {
        return 0;
    }
    return -1;
}

static int child_exit_code(int status)
{
    if (WIFEXITED(status)) {
        return WEXITSTATUS(status);
    }
    if (WIFSIGNALED(status)) {
        int code = 128 + WTERMSIG(status);
        return code <= 255 ? code : GUARDIAN_FAILURE;
    }
    return GUARDIAN_FAILURE;
}

/*
 * The child remains a direct child and is not considered gone until this
 * function's waitpid call reaps it. That makes kill(child_pid, ...) immune to
 * PID reuse during the shutdown window. No process group or name is touched.
 */
static int terminate_and_reap(pid_t child, int grace_ms, int *status)
{
    int64_t started = monotonic_ms();
    int64_t deadline = started < 0 ? -1 : started + grace_ms;
    int sent_term = 0;

    if (kill(child, SIGTERM) < 0 && errno != ESRCH) {
        report_generic("could not request child termination");
    }
    sent_term = 1;

    for (;;) {
        int waited = wait_for_child(child, status, 1);
        if (waited == 1) {
            return 0;
        }
        if (waited < 0 && errno == ECHILD) {
            report_generic("child was not waitable");
            return -1;
        }
        if (waited < 0) {
            report_generic("could not inspect child status");
            return -1;
        }

        int64_t now = monotonic_ms();
        if (deadline < 0 || now < 0 || now >= deadline) {
            break;
        }
        int64_t remaining = deadline - now;
        int timeout = remaining > POLL_INTERVAL_MS ? POLL_INTERVAL_MS : (int)remaining;
        if (poll(NULL, 0, timeout) < 0 && errno != EINTR) {
            report_generic("shutdown wait failed");
            break;
        }
    }

    if (sent_term && kill(child, SIGKILL) < 0 && errno != ESRCH) {
        report_generic("could not force child termination");
    }
    if (wait_for_child(child, status, 0) != 1) {
        report_generic("could not reap child");
        return -1;
    }
    return 0;
}

static int read_lifeline(void)
{
    unsigned char bytes[256];
    ssize_t count;

    do {
        count = read(STDIN_FILENO, bytes, sizeof(bytes));
    } while (count < 0 && errno == EINTR);
    if (count > 0) {
        return 1; /* Any owner-to-guardian bytes are an invalid protocol. */
    }
    if (count == 0) {
        return 0; /* EOF is the owner death/close signal. */
    }
    if (errno == EAGAIN || errno == EWOULDBLOCK) {
        return 2;
    }
    return -1;
}

/* Reject an owner that was already gone before any engine process is made. */
static int owner_is_live_before_spawn(void)
{
    struct pollfd descriptor = {STDIN_FILENO, POLLIN | POLLHUP | POLLERR, 0};
    int poll_result;

    do {
        poll_result = poll(&descriptor, 1, 0);
    } while (poll_result < 0 && errno == EINTR);
    if (poll_result < 0 || (descriptor.revents & POLLNVAL)) {
        return -1;
    }
    if (poll_result == 0) {
        return 1;
    }
    if (descriptor.revents & (POLLIN | POLLHUP | POLLERR)) {
        /* EOF and bytes are both invalid before spawn. A spurious readiness
         * result may race away and is treated as live only for EAGAIN. */
        return read_lifeline() == 2 ? 1 : 0;
    }
    return 1;
}

static int read_exec_error(int fd)
{
    int error_number = 0;
    ssize_t count;

    do {
        count = read(fd, &error_number, sizeof(error_number));
    } while (count < 0 && errno == EINTR);
    if (count > 0) {
        return 1;
    }
    if (count == 0) {
        return 0; /* CLOEXEC closed the pipe: exec succeeded. */
    }
    if (errno == EAGAIN || errno == EWOULDBLOCK) {
        return 2;
    }
    return -1;
}

static void child_exec(const char *path, char *const child_argv[], int error_read, int error_write,
                       const int *inherited_fds, size_t inherited_fd_count,
                       pid_t expected_parent)
{
    int devnull;
    struct sigaction default_pipe;

    close(error_read);
    if (error_write != 3) {
        if (dup2(error_write, 3) < 0) {
            child_report_and_exit(error_write, errno);
        }
        close(error_write);
    }
    error_write = 3;
    if (set_cloexec(error_write) < 0) {
        child_report_and_exit(error_write, errno);
    }

#if defined(__linux__)
    /* PR_SET_PDEATHSIG closes the guardian-death hole on Linux and Android.
     * Setting it before rechecking getppid closes the documented death race. */
    if (prctl(PR_SET_PDEATHSIG, SIGKILL) < 0) {
        child_report_and_exit(error_write, errno);
    }
    if (getppid() != expected_parent) {
        child_report_and_exit(error_write, ECHILD);
    }
#else
    (void)expected_parent;
#endif

    /* The guardian inventories descriptors after creating its status pipe and
     * opens nothing else before fork. The single-threaded child closes that
     * complete bounded inventory, regardless of a later lowered soft limit. */
    for (size_t index = 0; index < inherited_fd_count; index++) {
        int fd = inherited_fds[index];
        int result;
        do {
            result = close(fd);
        } while (result < 0 && errno == EINTR);
        if (result < 0 && errno != EBADF) {
            child_report_and_exit(error_write, errno);
        }
    }
    if (owner_is_live_before_spawn() != 1) {
        child_report_and_exit(error_write, ECHILD);
    }
    memset(&default_pipe, 0, sizeof(default_pipe));
    default_pipe.sa_handler = SIG_DFL;
    sigemptyset(&default_pipe.sa_mask);
    (void)sigaction(SIGPIPE, &default_pipe, NULL);
    devnull = open("/dev/null", O_RDONLY);
    if (devnull < 0) {
        child_report_and_exit(error_write, errno);
    }
    if (dup2(devnull, STDIN_FILENO) < 0) {
        child_report_and_exit(error_write, errno);
    }
    if (devnull != STDIN_FILENO) {
        close(devnull);
    }
    execv(path, child_argv);
    child_report_and_exit(error_write, errno);
}

static int usage(void)
{
    write_literal("usage: engine-guardian [--grace-ms N] -- /absolute/engine [args...]\n");
    return 64;
}

int main(int argc, char **argv)
{
    int grace_ms = DEFAULT_GRACE_MS;
    int child_start = -1;
    int error_pipe[2];
    pid_t child;
    int error_fd;
    int status = 0;
    int shutdown_reason = 0;
    int *inherited_fds = NULL;
    size_t inherited_fd_count = 0;
    pid_t guardian_pid;

    for (int index = 1; index < argc; index++) {
        if (strcmp(argv[index], "--") == 0) {
            child_start = index + 1;
            break;
        }
        if (strcmp(argv[index], "--grace-ms") == 0 && index + 1 < argc) {
            if (parse_grace_ms(argv[++index], &grace_ms) < 0) {
                return usage();
            }
            continue;
        }
        return usage();
    }
    if (child_start < 0 || child_start >= argc || argv[child_start][0] != '/') {
        return usage();
    }

    if (install_signal_handlers() < 0) {
        report_generic("could not install signal handlers");
        return GUARDIAN_FAILURE;
    }
    if (set_nonblocking(STDIN_FILENO) < 0) {
        report_generic("owner lifeline could not be made nonblocking");
        return GUARDIAN_FAILURE;
    }
    if (owner_is_live_before_spawn() != 1) {
        report_generic("owner lifeline unavailable before spawn");
        return GUARDIAN_FAILURE;
    }
    if (stop_signal != 0) {
        report_generic("shutdown requested before spawn");
        return GUARDIAN_FAILURE;
    }
    if (pipe(error_pipe) < 0 || set_cloexec(error_pipe[0]) < 0 || set_cloexec(error_pipe[1]) < 0) {
        report_generic("could not create exec status pipe");
        return GUARDIAN_FAILURE;
    }
    if (collect_inherited_descriptors(&inherited_fds, &inherited_fd_count) < 0) {
        close(error_pipe[0]);
        close(error_pipe[1]);
        report_generic("could not inventory inherited descriptors");
        return GUARDIAN_FAILURE;
    }

    guardian_pid = getpid();
    child = fork();
    if (child < 0) {
        free(inherited_fds);
        close(error_pipe[0]);
        close(error_pipe[1]);
        report_generic("could not spawn engine");
        return GUARDIAN_FAILURE;
    }
    if (child == 0) {
        child_exec(argv[child_start], &argv[child_start], error_pipe[0], error_pipe[1],
                   inherited_fds, inherited_fd_count, guardian_pid);
    }
    free(inherited_fds);

    close(error_pipe[1]);
    error_fd = error_pipe[0];
    for (;;) {
        struct pollfd descriptors[2];
        nfds_t descriptor_count = 1;
        int poll_result;
        int waited;

        if (stop_signal != 0 || child_changed != 0) {
            if (child_changed != 0) {
                child_changed = 0;
            }
        }
        if (stop_signal != 0) {
            shutdown_reason = stop_signal;
            break;
        }
        waited = wait_for_child(child, &status, 1);
        if (waited == 1) {
            close(error_fd);
            return child_exit_code(status);
        }
        if (waited < 0) {
            report_generic("could not wait for engine");
            shutdown_reason = SIGTERM;
            break;
        }

        descriptors[0].fd = STDIN_FILENO;
        descriptors[0].events = POLLIN | POLLHUP | POLLERR;
        descriptors[0].revents = 0;
        if (error_fd >= 0) {
            descriptors[1].fd = error_fd;
            descriptors[1].events = POLLIN | POLLHUP | POLLERR;
            descriptors[1].revents = 0;
            descriptor_count = 2;
        }
        do {
            poll_result = poll(descriptors, descriptor_count, POLL_INTERVAL_MS);
        } while (poll_result < 0 && errno == EINTR);
        if (poll_result < 0) {
            report_generic("lifeline poll failed");
            shutdown_reason = SIGTERM;
            break;
        }
        if (poll_result == 0) {
            continue;
        }

        if (error_fd >= 0 && descriptors[1].revents & (POLLIN | POLLHUP | POLLERR | POLLNVAL)) {
            int exec_result = read_exec_error(error_fd);
            if (exec_result == 1) {
                report_generic("engine could not be executed");
                shutdown_reason = EXEC_FAILURE;
                break;
            }
            if (exec_result == 0 || exec_result < 0 || (descriptors[1].revents & POLLNVAL)) {
                close(error_fd);
                error_fd = -1;
            }
        }
        /* Prefer a pending pre-exec error when EOF arrives at the same time. */
        if (descriptors[0].revents & (POLLIN | POLLHUP | POLLERR | POLLNVAL)) {
            int lifeline = read_lifeline();
            if (lifeline == 1) {
                report_generic("unexpected bytes on owner lifeline");
                shutdown_reason = GUARDIAN_FAILURE;
                break;
            }
            if (lifeline == 0 && error_fd >= 0) {
                /* EOF can race a just-started exec failure; give the
                 * close-on-exec status pipe one short, bounded chance to
                 * report that precise error before owner shutdown wins. */
                struct pollfd exec_status = {error_fd, POLLIN | POLLHUP | POLLERR, 0};
                int status_poll;
                do {
                    status_poll = poll(&exec_status, 1, 50);
                } while (status_poll < 0 && errno == EINTR);
                if (status_poll > 0) {
                    int exec_result = read_exec_error(error_fd);
                    if (exec_result == 1) {
                        report_generic("engine could not be executed");
                        shutdown_reason = EXEC_FAILURE;
                        break;
                    }
                    if (exec_result == 0 || exec_result < 0) {
                        close(error_fd);
                        error_fd = -1;
                    }
                }
            }
            if (lifeline == 0 || lifeline < 0 || (descriptors[0].revents & POLLNVAL)) {
                shutdown_reason = SIGTERM;
                break;
            }
            if (lifeline == 2 && (descriptors[0].revents & POLLERR)) {
                report_generic("owner lifeline reported an error");
                shutdown_reason = SIGTERM;
                break;
            }
        }
    }

    if (error_fd >= 0) {
        close(error_fd);
    }
    if (terminate_and_reap(child, grace_ms, &status) < 0) {
        return GUARDIAN_FAILURE;
    }
    if (shutdown_reason == EXEC_FAILURE) {
        return EXEC_FAILURE;
    }
    return GUARDIAN_FAILURE;
}
