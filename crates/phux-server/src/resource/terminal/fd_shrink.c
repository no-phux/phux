/* Shrink the fd table, then become the pane command.
 *
 * Linux fork sizes the child's fd table to the highest open fd, rounded up
 * to a power of two. Closing the spare fds does not shrink that allocation,
 * so a server with thousands of PTYs was paying hundreds of kilobytes per
 * idle shell. unshare(CLONE_FILES) is a no-op while this process is the
 * only holder of its files_struct. A CLONE_FILES sibling bumps the refcount,
 * unshare copies a table sized to the fds still open, and the sibling's
 * exit frees the fat table. The following exec keeps the small one.
 *
 * argv is: phux-fd-shrink -- program arg...
 * The program replaces this process, so the server's child pid is the shell.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#ifdef __linux__
#include <sched.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#endif

#ifdef __linux__
static int hold_fd_table(void *arg) {
    (void)arg;
    for (;;) {
        pause();
    }
}

#endif

/* fd 3 is explicitly duplicated by posix_spawn, never an inherited upgrade fd.
 * Ready + EOF means exec closed it; ready + stage + big-endian errno is failure.
 * Only fixed-size evidence crosses this channel; no argv/environment text. */
static void status_write(const unsigned char *bytes, size_t size) {
    while (size > 0) {
        ssize_t n = write(3, bytes, size);
        if (n < 0 && errno == EINTR) continue;
        if (n <= 0) _exit(127);
        bytes += n;
        size -= (size_t)n;
    }
}

static void launch_failed(unsigned char stage) {
    uint32_t error = (uint32_t)errno;
    const unsigned char bytes[] = {
        stage, (unsigned char)(error >> 24), (unsigned char)(error >> 16),
        (unsigned char)(error >> 8), (unsigned char)error,
    };
    status_write(bytes, sizeof(bytes));
    _exit(127);
}

static void reset_signals(void) {
    const int caught[] = {
        SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGALRM, SIGCHLD,
        SIGTSTP, SIGTTIN, SIGTTOU, SIGWINCH,
    };
    size_t i;
    sigset_t empty;

    for (i = 0; i < sizeof(caught) / sizeof(caught[0]); i++) {
        signal(caught[i], SIG_DFL);
    }
    sigemptyset(&empty);
    sigprocmask(SIG_SETMASK, &empty, 0);
}

static void shrink_linux_fd_table(void) {
#ifdef __linux__
    static char stack[8192] __attribute__((aligned(16)));
    pid_t sibling;
    int status;

#ifdef SYS_close_range
    /* Ignore failure: kernels without the syscall still have the fds
     * portable-pty or CLOEXEC already closed. */
    (void)syscall(SYS_close_range, 4, ~0U, 0);
#endif
    sibling = clone(hold_fd_table, stack + sizeof(stack), CLONE_FILES | SIGCHLD, 0);
    if (sibling < 0) {
        return;
    }
    if (unshare(CLONE_FILES) != 0) {
        kill(sibling, SIGKILL);
        while (waitpid(sibling, &status, 0) < 0 && errno == EINTR) {
        }
        return;
    }
    kill(sibling, SIGKILL);
    while (waitpid(sibling, &status, 0) < 0 && errno == EINTR) {
    }
#else
    (void)0;
#endif
}

static void apply_spawn_dir(void) {
    const char *cwd = getenv("PHUX_FD_SHRINK_CWD");
    if (cwd == 0 || cwd[0] == '\0') {
        return;
    }
    if (chdir(cwd) != 0) {
        launch_failed(1);
    }
    unsetenv("PHUX_FD_SHRINK_CWD");
}

int main(int argc, char **argv) {
    const unsigned char ready = 0xa5;
    status_write(&ready, 1);
    if (fcntl(3, F_SETFD, FD_CLOEXEC) < 0) launch_failed(3);
    if (argc < 3 || strcmp(argv[1], "--") != 0 || argv[2][0] == '\0') {
        errno = EINVAL;
        launch_failed(2);
    }
    shrink_linux_fd_table();
    reset_signals();
    if (setsid() < 0) launch_failed(3);
    if (ioctl(0, TIOCSCTTY, 0) < 0) launch_failed(3);
    apply_spawn_dir();
    execvp(argv[2], &argv[2]);
    launch_failed(2);
}
