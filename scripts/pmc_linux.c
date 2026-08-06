// Linux twin of scripts/pmc.c — exact hardware counters for a child command.
//
// Same contract as the macOS version: counters go to stderr, the child's
// stdout passes through untouched, so a sink check still works in the SAME
// run as the measurement. Exit status is the child's.
//
//   cc -O2 -o pmc_linux scripts/pmc_linux.c
//   ./pmc_linux ./two_sum_iii
//   -> 762965
//   -> instructions=... cycles=... branch-misses=... cache-misses=... ipc=...
//
// Why this and not `perf stat` or callgrind:
//   * `perf` the CLI is often absent in containers; this needs only the
//     perf_event_open syscall, which is in the kernel, not a package.
//   * callgrind counts SIMULATED instructions and reports no cycles at all,
//     so it cannot produce an IPC number — and IPC is what explains a change
//     that cuts instructions while costing wall time (B-2026-08-05-5).
//
// Counting matches pmc.c's semantics deliberately, so the two hosts are
// comparable: the child's whole run, user AND kernel, threads inherited.
// `enable_on_exec` means none of this wrapper's own setup is counted.
//
// Needs /proc/sys/kernel/perf_event_paranoid <= 2 (the default on most
// distros). No root otherwise. If it is 3, or the container drops
// CAP_PERFMON, perf_event_open returns EACCES/EPERM and we say so.
//
// AND IT NEEDS A REAL PMU. In a VM without a virtualized PMU the hardware
// events simply do not exist and perf_event_open returns ENOENT no matter
// how permissive the paranoid setting is — verified in a colima/Lima
// aarch64 guest, where paranoid=0 still yields ENOENT. Check this FIRST on
// any cloud/CI box before concluding a measurement is impossible.
//
// LINUX ONLY. A macOS editor/clang will flag <linux/perf_event.h> as missing
// in this file; that is expected. The macOS equivalent is scripts/pmc.c,
// which uses libproc and prints the same two leading keys.

#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <linux/perf_event.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>

struct counter {
    const char *name;
    uint32_t type;
    uint64_t config;
    int fd;
    uint64_t value, enabled, running;
    int ok, scaled;
};

static struct counter counters[] = {
    {"instructions",  PERF_TYPE_HARDWARE, PERF_COUNT_HW_INSTRUCTIONS,  -1, 0, 0, 0, 0, 0},
    {"cycles",        PERF_TYPE_HARDWARE, PERF_COUNT_HW_CPU_CYCLES,    -1, 0, 0, 0, 0, 0},
    {"branch-misses", PERF_TYPE_HARDWARE, PERF_COUNT_HW_BRANCH_MISSES, -1, 0, 0, 0, 0, 0},
    {"cache-misses",  PERF_TYPE_HARDWARE, PERF_COUNT_HW_CACHE_MISSES,  -1, 0, 0, 0, 0, 0},
};
static const int NCOUNTERS = (int)(sizeof(counters) / sizeof(counters[0]));

static long perf_event_open_(struct perf_event_attr *attr, pid_t pid, int cpu,
                             int group_fd, unsigned long flags) {
    return syscall(SYS_perf_event_open, attr, pid, cpu, group_fd, flags);
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <command> [args...]\n", argv[0]);
        return 2;
    }

    // Sync pipe: the child must exist (so we can attach counters to its pid)
    // but must not exec until the counters are armed.
    int sync_pipe[2];
    if (pipe(sync_pipe) != 0) { perror("pipe"); return 2; }

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 2; }

    if (pid == 0) {
        close(sync_pipe[1]);
        char go;
        // Blocks until the parent has armed the counters (or closed the pipe).
        while (read(sync_pipe[0], &go, 1) < 0 && errno == EINTR) { }
        close(sync_pipe[0]);
        execvp(argv[1], &argv[1]);
        perror("execvp");
        _exit(127);
    }

    close(sync_pipe[0]);

    int opened = 0, first_errno = 0;
    for (int i = 0; i < NCOUNTERS; i++) {
        struct perf_event_attr attr;
        memset(&attr, 0, sizeof(attr));
        attr.size = sizeof(attr);
        attr.type = counters[i].type;
        attr.config = counters[i].config;
        attr.disabled = 1;        // armed here, started by exec
        attr.enable_on_exec = 1;  // excludes this wrapper's setup entirely
        attr.inherit = 1;         // count threads the child spawns
        attr.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;

        long fd = perf_event_open_(&attr, pid, -1, -1, 0);
        if (fd < 0) {
            if (!first_errno) first_errno = errno;
            counters[i].fd = -1;
        } else {
            counters[i].fd = (int)fd;
            counters[i].ok = 1;
            opened++;
        }
    }

    if (opened == 0) {
        fprintf(stderr, "pmc_linux: perf_event_open failed for every counter: %s\n",
                strerror(first_errno));
        if (first_errno == ENOENT) {
            // Verified 2026-08-05 in a colima/Lima aarch64 guest: with
            // perf_event_paranoid=0 the error is ENOENT, not EPERM, because
            // Apple's Virtualization framework exposes no virtual PMU. Same
            // shape on most cloud VMs. Loosening permissions cannot fix this.
            fprintf(stderr, "pmc_linux: ENOENT means the HARDWARE events do not exist here — "
                            "typically a VM with no virtualized PMU. This is not a permissions "
                            "problem and paranoid/CAP_PERFMON will not fix it; you need a host "
                            "that exposes a PMU (bare metal, or a hypervisor with vPMU on).\n");
        } else {
            fprintf(stderr, "pmc_linux: check /proc/sys/kernel/perf_event_paranoid (need <= 2) "
                            "and whether the container grants CAP_PERFMON.\n");
        }
    }

    // Release the child; enable_on_exec starts the counters at its execvp.
    if (write(sync_pipe[1], "g", 1) < 0) perror("write");
    close(sync_pipe[1]);

    int status = 0;
    while (waitpid(pid, &status, 0) < 0 && errno == EINTR) { }

    for (int i = 0; i < NCOUNTERS; i++) {
        if (!counters[i].ok) continue;
        uint64_t buf[3] = {0, 0, 0};
        ssize_t n = read(counters[i].fd, buf, sizeof(buf));
        close(counters[i].fd);
        if (n < (ssize_t)sizeof(buf)) { counters[i].ok = 0; continue; }
        counters[i].value = buf[0];
        counters[i].enabled = buf[1];
        counters[i].running = buf[2];
        // The kernel multiplexes when events outnumber PMU slots; scale back
        // up and SAY SO, because a silently scaled count is an estimate.
        if (counters[i].running > 0 && counters[i].running < counters[i].enabled) {
            counters[i].value = (uint64_t)((double)counters[i].value *
                                           (double)counters[i].enabled /
                                           (double)counters[i].running);
            counters[i].scaled = 1;
        }
    }

    int any_scaled = 0;
    for (int i = 0; i < NCOUNTERS; i++) {
        if (counters[i].ok) {
            fprintf(stderr, "%s=%llu ", counters[i].name,
                    (unsigned long long)counters[i].value);
            any_scaled |= counters[i].scaled;
        } else {
            fprintf(stderr, "%s=unavailable ", counters[i].name);
        }
    }
    if (counters[0].ok && counters[1].ok && counters[1].value > 0) {
        fprintf(stderr, "ipc=%.3f ", (double)counters[0].value / (double)counters[1].value);
    }
    if (any_scaled) {
        fprintf(stderr, "[MULTIPLEXED — counts scaled, treat as estimates; "
                        "re-run with fewer counters for exact values]");
    }
    fprintf(stderr, "\n");

    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
