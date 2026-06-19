// Relay bench — Go reference impl: idiomatic single-host reverse proxy.
//
// `httputil.NewSingleHostReverseProxy` — the standard-library reverse proxy,
// the way a Go engineer would write it. It owns connection pooling, header
// rewriting, and the goroutine-per-connection model. That goroutine lifecycle
// is exactly the differentiator framing: in Kāra you never write it; here the
// runtime spins one per connection and you trust the scheduler.
//
// **Upstream connection pooling — load-bearing for a fair benchmark.** The
// proxy's default transport (`http.DefaultTransport`) caps reusable idle
// connections per host at `MaxIdleConnsPerHost = 2`. Under concurrent load that
// means almost every request opens a *fresh* upstream connection and discards
// it into TIME_WAIT, which exhausts the host's ephemeral ports on loopback
// (`connect: can't assign requested address`) and collapses throughput to a
// few hundred req/s with 502s — a benchmark artifact, not a real Go ceiling. A
// production Go reverse proxy pools upstream connections; we do the same here
// with an explicit `Transport`, so the comparison measures the proxy and not
// the default transport's idle-pool cap. (The Node comparator already pools via
// `new http.Agent({ keepAlive: true, maxSockets: 4096 })`; the Kāra proxy holds
// one persistent upstream connection per client connection.) See the bench
// README's "How to read this" section for the before/after this fix produced.
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
	"time"
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
	// Pool upstream keep-alive connections instead of the default
	// MaxIdleConnsPerHost=2 (see the package comment). Clone the default
	// transport so dialer timeouts etc. are preserved, then raise the idle
	// pool well above the connection sweep's high-water mark.
	tr := http.DefaultTransport.(*http.Transport).Clone()
	tr.MaxIdleConns = 10000
	tr.MaxIdleConnsPerHost = 10000
	tr.IdleConnTimeout = 90 * time.Second
	proxy.Transport = tr

	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fmt.Println("bind failed:", err)
		return
	}
	addr := listener.Addr().(*net.TCPAddr)
	fmt.Printf("BOUND_PORT=%d\n", addr.Port)
	_ = http.Serve(listener, proxy)
}
