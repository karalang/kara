// Same task in Go — sends welcomes concurrently, counts them.
//
// `go build` compiles it clean. `go vet ./...` reports nothing. The
// program ships. The shared `sentCount++` (load-add-store) races
// across goroutines: the final count is non-deterministic and can
// under-count.
//
// The race is caught ONLY by the opt-in runtime detector
// (`go run -race .`), and only on a scheduling interleaving that
// happens to expose it — never at compile time. Verified on
// go1.26.3: `go vet` and `go build` pass; `go run -race` prints
// "WARNING: DATA RACE". See notes.md.
package main

import (
	"fmt"
	"sync"
)

var sentCount int

func sendWelcome(userID int) {
	fmt.Println("welcome user")
	sentCount++ // <-- data race: not atomic
}

func main() {
	var wg sync.WaitGroup
	for _, id := range []int{1, 2, 3} {
		wg.Add(1)
		go func(uid int) {
			defer wg.Done()
			sendWelcome(uid)
		}(id)
	}
	wg.Wait()
	fmt.Printf("done: sent %d\n", sentCount)
}
