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
	"os"
	"strconv"
)

// Response body. Defaults to a tiny "OK" (the local-bench default, where a
// 2-byte body keeps the upstream's per-request cost dominated by HTTP framing).
// `RELAY_BODY_BYTES` overrides the size: the cross-host harness sweeps larger
// payloads (e.g. 1 KiB / 16 KiB) so the proxy stays the bottleneck instead of
// the test going network-bound on 2 bytes across a real wire.
var body = makeBody()

func makeBody() []byte {
	if s := os.Getenv("RELAY_BODY_BYTES"); s != "" {
		if n, err := strconv.Atoi(s); err == nil && n >= 0 {
			b := make([]byte, n)
			for i := range b {
				b[i] = 'x'
			}
			return b
		}
	}
	return []byte("OK")
}

// Listen address: `RELAY_UPSTREAM_BIND` env var, or `127.0.0.1:0` (ephemeral
// loopback, the local-bench default). The cross-host harness sets it to a
// routable `0.0.0.0:<port>` so the proxy host can reach the upstream host.
func bindAddr() string {
	if a := os.Getenv("RELAY_UPSTREAM_BIND"); a != "" {
		return a
	}
	return "127.0.0.1:0"
}

func handle(w http.ResponseWriter, r *http.Request) {
	_ = r // path/body ignored — the backend is intentionally uniform
	w.Header().Set("content-type", "text/plain")
	w.Header().Set("content-length", fmt.Sprintf("%d", len(body)))
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(body)
}

func main() {
	listener, err := net.Listen("tcp", bindAddr())
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
