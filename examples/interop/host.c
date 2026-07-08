/* A plain C host that links the Kāra kernel through its emitted C ABI.
 *
 * Build the library first, then compile this against ONLY the emitted
 * `.a` + `.h` — no karac toolchain on the link line:
 *
 *   karac build kernel.kara --crate-type staticlib
 *   cc host.c -L. -l:libkernel.a -lpthread -lm -ldl -o host_c
 *   ./host_c        # => add=42 fib=6765 mean=7.50
 *
 * The static archive bundles the Kāra runtime, so the C program is
 * self-contained — it runs with no karac install present.
 */
#include <stdio.h>
#include "libkernel.h"

int main(void) {
    karac_runtime_init();

    struct Stats s = { .sum = 30.0, .count = 4 };
    printf("add=%d fib=%lld mean=%.2f\n",
           add(20, 22),
           (long long)fib(20),
           stats_mean(s));

    karac_runtime_shutdown();
    return 0;
}
