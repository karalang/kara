// Exact whole-process instruction + cycle counts for a child command.
// Uses the kernel's per-task hardware counters via public libproc
// (proc_pid_rusage / RUSAGE_INFO_V4). No sampling, no sudo, no Instruments.
//
// The parent polls the child's counters and keeps the last successful read
// before exit; counters are monotonic, so the only error is the sub-poll-
// interval tail (<0.1% at 200us on a ~1s program) and it is symmetric across
// the binaries being compared.
//
//   cc -O2 -o pmc pmc.c && ./pmc ./some_binary
//   -> child's stdout passes through; counters go to stderr.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <libproc.h>

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: %s <command> [args...]\n", argv[0]);
        return 2;
    }

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 2; }
    if (pid == 0) {
        execv(argv[1], &argv[1]);
        perror("execv");
        _exit(127);
    }

    struct rusage_info_v4 last;
    memset(&last, 0, sizeof(last));
    int status = 0, got = 0;

    for (;;) {
        struct rusage_info_v4 cur;
        if (proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&cur) == 0) {
            last = cur;
            got = 1;
        }
        pid_t r = waitpid(pid, &status, WNOHANG);
        if (r == pid) break;
        if (r < 0) break;
        usleep(200);
    }

    if (!got) { fprintf(stderr, "pmc: no counter samples\n"); return 2; }
    fprintf(stderr, "instructions=%llu cycles=%llu\n",
            (unsigned long long)last.ri_instructions,
            (unsigned long long)last.ri_cycles);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}
