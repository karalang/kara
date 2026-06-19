// Relay bench — shared upstream backend.
//
// One fixed, fast HTTP origin server that ALL three proxies (kara, go, node)
// forward to, so the *proxy* is the thing under test, not the backend. It
// returns a small constant body for every request and does no per-request
// work beyond writing the response, so it out-throughputs every proxy and is
// never the bottleneck.
//
// **BOUND_PORT convention.** Binds `127.0.0.1:0` and prints
// `BOUND_PORT=<n>\n` to stdout, mirroring the Kāra runtime's convention so
// `bench.sh` discovers the ephemeral port with one helper. `bench.sh` then
// exports `RELAY_UPSTREAM=127.0.0.1:<n>` to each proxy.
//
// Go is the implementation language because the toolchain is already present
// and a single static binary is trivially fast — but it is the *backend*, not
// a comparator; the Go *proxy* lives in `../go/`.
package main

import (
	"fmt"
	"net"
	"net/http"
)

// Small constant body. Keeping it tiny (and identical across every response)
// means the upstream's per-request cost is dominated by the HTTP framing, not
// payload generation — exactly what we want from a backend that must not be
// the bottleneck.
var body = []byte("OK")

func handle(w http.ResponseWriter, r *http.Request) {
	_ = r // path/body ignored — the backend is intentionally uniform
	w.Header().Set("content-type", "text/plain")
	w.Header().Set("content-length", fmt.Sprintf("%d", len(body)))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(body)
}

func main() {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fmt.Println("bind failed:", err)
		return
	}
	addr := listener.Addr().(*net.TCPAddr)
	fmt.Printf("BOUND_PORT=%d\n", addr.Port)
	mux := http.NewServeMux()
	mux.HandleFunc("/", handle)
	_ = http.Serve(listener, mux)
}
