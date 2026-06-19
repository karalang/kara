// Relay bench — Go reference impl: idiomatic single-host reverse proxy.
//
// `httputil.NewSingleHostReverseProxy` — the standard-library reverse proxy,
// the way a Go engineer would write it. It owns connection pooling, header
// rewriting, and the goroutine-per-connection model. That goroutine lifecycle
// is exactly the differentiator framing: in Kāra you never write it; here the
// runtime spins one per connection and you trust the scheduler.
//
// **Upstream address.** Read from `RELAY_UPSTREAM` (an `IP:port` literal),
// defaulting to `127.0.0.1:9000`. `bench.sh` exports it pointed at the shared
// upstream's discovered ephemeral port.
//
// **BOUND_PORT convention.** Binds `127.0.0.1:0` and prints `BOUND_PORT=<n>`
// so `bench.sh`'s one port-discovery helper works across all three impls.
package main

import (
	"fmt"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"os"
)

func upstreamAddr() string {
	if a := os.Getenv("RELAY_UPSTREAM"); a != "" {
		return a
	}
	return "127.0.0.1:9000"
}

func main() {
	target, err := url.Parse("http://" + upstreamAddr())
	if err != nil {
		fmt.Println("bad upstream:", err)
		return
	}
	proxy := httputil.NewSingleHostReverseProxy(target)

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fmt.Println("bind failed:", err)
		return
	}
	addr := listener.Addr().(*net.TCPAddr)
	fmt.Printf("BOUND_PORT=%d\n", addr.Port)
	_ = http.Serve(listener, proxy)
}
