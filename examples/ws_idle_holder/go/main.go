// Go reference impl ("comparator") for the `ws_idle_holder` flagship
// demo. Mirrors `../src/main.kara` and the sibling `../rust` Rust
// comparator: binds a TLS listener on 127.0.0.1:<ephemeral>, prints
// `BOUND_PORT=<n>` to stdout so the `../bench` harness can read it back,
// and accepts WebSocket-over-TLS connections, echoing any frame it
// receives and holding each connection idle otherwise until the peer
// closes.
//
// # Why this exists
//
// docs/implementation_checklist/phase-6-runtime.md line 170 names the
// ws-idle-holder workload as the runtime's M1/M2/M3 scale target. Go
// (gorilla/websocket + crypto/tls) is the first *commercial-stack*
// comparator in the bench-day Phase 3 sweep — gorilla/websocket is the
// raw-library Go prod default (the "what a competent Go shop ships"
// baseline, not a framework like nhooyr/websocket-over-a-router or a
// full RPC stack). Both this server and the Kāra impl traverse the same
// kernel surface — accept(2) -> TLS handshake -> RFC 6455 upgrade — so
// the comparison isolates language-runtime overhead from the IO
// substrate.
//
// # Design choices (vs. the Kāra impl)
//
//   - Concurrency model: net/http's goroutine-per-connection via
//     http.Server.ServeTLS. This is the idiomatic gorilla/websocket
//     server shape — an http.Handler that calls upgrader.Upgrade — i.e.
//     what a competent Go developer would write, NOT a hand-tuned
//     mirror of Kāra's internal handshake-worker pool. Go's scheduler
//     multiplexes the goroutines across GOMAXPROCS OS threads, the
//     analogue of tokio's multi-thread runtime in the Rust comparator.
//   - WS upgrade: gorilla/websocket's Upgrader drives the RFC 6455
//     server-side handshake. Equivalent to the Kāra runtime's
//     hand-rolled ws_drive_upgrade_handshake in
//     runtime/src/event_loop.rs and to tokio-tungstenite's accept_async
//     in the Rust comparator.
//   - TLS: crypto/tls (Go stdlib) with MinVersion = TLS 1.2 (MaxVersion
//     defaults to TLS 1.3), no client auth, single cert. This mirrors
//     the rustls "TLS 1.2 + 1.3, no client auth, single cert" posture
//     in runtime/src/tls.rs / the Rust comparator. crypto/tls is Go's
//     native stack — no OpenSSL/BoringSSL cgo link — which is the
//     real-world Go prod default and keeps the build pure-Go.
//   - Listen backlog: idiomatic net.Listen. Go's runtime derives the
//     listen(2) backlog from /proc/sys/net/core/somaxconn on Linux
//     (kern.ipc.somaxconn on macOS), so a Go dev raises the sysctl
//     rather than hand-coding a backlog. ../bench/scripts/ec2_setup.sh
//     sets net.core.somaxconn=65535, which makes Go's auto-backlog match
//     the Rust comparator's explicit socket2 listen(65535) on the rig.
//     (On macOS the default somaxconn is 128 — fine for local
//     validation at a few hundred conns; the at-scale runs are Linux.)
//   - TCP_NODELAY: Go enables it by default on all TCP conns, matching
//     the Rust comparator's explicit set_nodelay(true) — the WS upgrade
//     response goes out without Nagle delay.
//   - Cert + key: inlined as PEM string constants below, exactly as
//     ../src/main.kara inlines them. The Rust comparator include_str!'s
//     ../../../../tests/fixtures/tls/{cert,key}.pem; Go's //go:embed
//     cannot reference a parent directory (no ".." in embed patterns),
//     so we inline the same committed-fixture bytes here — the truest
//     mirror of the Kāra demo, which also inlines them. These are the
//     v1 self-signed test fixtures (CN=localhost, valid through 2036)
//     already committed at tests/fixtures/tls/, so inlining exposes
//     nothing not already in the repo.
//
// # Echo on message (Phase 2 parity)
//
// The per-connection handler echoes any text/binary frame straight back,
// mirroring the Kāra demo's handle_connection and the Rust comparator.
// This lets the active-traffic bench drive request/response load through
// all three impls identically. Idle connections send nothing, so the
// echo branch is never reached on an idle hold — the per-connection
// density numbers are unchanged, and the same binary serves both idle
// and active loads (no cross-binary confound).
//
// # What this impl deliberately omits
//
//   - No structured logging (http.Server.ErrorLog is silenced so a
//     storm of bad-handshake errors doesn't spam stderr / pollute the
//     harness's human-readable channel).
//   - No graceful shutdown / max-conn cap.
//   - No connection-attempt rate limiting.
package main

import (
	"crypto/tls"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"

	"github.com/gorilla/websocket"
)

// Self-signed test cert + key (CN=localhost, valid through 2036) — the
// same bytes committed at tests/fixtures/tls/{cert,key}.pem and inlined
// by ../src/main.kara. See the package doc for why these are inlined
// rather than embedded from the shared fixture.
const certPEM = `-----BEGIN CERTIFICATE-----
MIIDJTCCAg2gAwIBAgIUQOf5oAPYR25vaDHir+RzYUvNJBcwDQYJKoZIhvcNAQEL
BQAwFDESMBAGA1UEAwwJbG9jYWxob3N0MB4XDTI2MDUyODAyMzA1N1oXDTM2MDUy
ODAyMzA1N1owFDESMBAGA1UEAwwJbG9jYWxob3N0MIIBIjANBgkqhkiG9w0BAQEF
AAOCAQ8AMIIBCgKCAQEA3Rl880yaXgbA4GkBhV/hMmTQeA9uFcuYeB3jV2TWMrYD
oQWc8S3wB45xZ74A29sT+sZpJcPnjOuao8OLpU4JSnoidAS4S5080BbESrxG8Ee5
iffCcbwet+tzS+GFbvm4jAswTD/L9Z333OIxjxE6sgSPi3scECNdq3lugIEAWINE
mmiOxX9JXy7Bq2OEmubhhA7RAmR0EgYsVXbCxZMSu7XhCYvLQhXtpyL6Y39CPIFz
wfosDYl9/1I2YVDtpt151brppuLIVbmdnJwalb4RrZjc+FD/FVn2YiQ3sor+7FL2
nQ6pufzwwpaV2Qn0zjhXXnESyxl8UmAK9ZXKuAMGsQIDAQABo28wbTAdBgNVHQ4E
FgQUG/7G6tRhc+bu0vPOCGxlRlL3F+4wHwYDVR0jBBgwFoAUG/7G6tRhc+bu0vPO
CGxlRlL3F+4wDwYDVR0TAQH/BAUwAwEB/zAaBgNVHREEEzARgglsb2NhbGhvc3SH
BH8AAAEwDQYJKoZIhvcNAQELBQADggEBAJrXCRKxpWQlyODUwOPzPs/OI6CG7H2b
/rz1E7zrovPJndxqqtkSfjKk9xWF5zZj+gAwB9c/5HgdFTj8yw+DyqqSmd73SGMW
+z0QVfQ+yUoDxAafr6nymX/KyjmHIF6qrJuPKGWLQEqWNmEQEUMj+YOEbsrGu/jA
4UrGq7C2riM4kJy1pYnUak1CpkIea2PJ3/92VGN3D9fnXz19uve4hKxi4Tn36Pxr
pEZzXaNLK0WDuPAlRSyuAh/ZZodYYXkI2xYj/SoDJBBZ46E7gcH1550c7Oe5uEV4
t+59FVL1vxM53WBT9uZ+vqmkXQjFFKcYdb4Tf99yVN1NNDCPH+6H+yY=
-----END CERTIFICATE-----
`

const keyPEM = `-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDdGXzzTJpeBsDg
aQGFX+EyZNB4D24Vy5h4HeNXZNYytgOhBZzxLfAHjnFnvgDb2xP6xmklw+eM65qj
w4ulTglKeiJ0BLhLnTzQFsRKvEbwR7mJ98JxvB6363NL4YVu+biMCzBMP8v1nffc
4jGPETqyBI+LexwQI12reW6AgQBYg0SaaI7Ff0lfLsGrY4Sa5uGEDtECZHQSBixV
dsLFkxK7teEJi8tCFe2nIvpjf0I8gXPB+iwNiX3/UjZhUO2m3XnVuumm4shVuZ2c
nBqVvhGtmNz4UP8VWfZiJDeyiv7sUvadDqm5/PDClpXZCfTOOFdecRLLGXxSYAr1
lcq4AwaxAgMBAAECggEAMGno3OOst6MV7+2+WgStLJo7tdZ3HgmnWMH+qn/XkWIe
uE8g1wTeluD/fx5xVLMLlHGGy7Cyjr52bZ6fgPJuAWNuEOaJrnD/RHd/wveoNuwV
uhrI9puhRFentvlqfOrsmKnIiSG9GQren/zdqjy1FA8AmaO6+OOtmqMr6bKVr0ui
7Ko16ahrdFg2GKEbbrEIpUsV5ORAU7VX/7ycj+CwVHz/CgXGP7/4T+vF3iFdMAqu
MVFeZGW4irNL+UGH7g4YFUygpY00eK7eN2jOqTkoxqIiaitNKbNJHuqR8h/OcT+4
2XS/mL6os3m4KXATJ3nKESf46Wb+IqlnTZ87768vkwKBgQDzxxIcJjHqCU+cP7Qs
hkVhQBamD5RvudwLkPRfqN1VVt8lAtXOd8NMg41tRCb8Mus3ram7y1qthng5yvFd
R4PwyXikJtZuwJjcqOnII3SzU0ENFSNfiDJ1CXr/eXgJkY82lxc5yQ+kCzTJbv+J
hHE8NhilPGogElfzZQcAzRlnwwKBgQDoL1cmqCT92VV958m1ixC0N07154JHeUDH
qzYlfLcpyj+OeUbjvmlItB+8mBEZTHWFw4dcgYJ4zaAz5YZ6BL6ZrQMc9SfZNuC/
ssC+Y+lXOJD3ucLJsjvanA3BG5lv8aCB14neWznABp/t/A71HwAnnoLLa4adfCYP
9vr0gEJkewKBgBhrd6f0N4nPNvda9kyDgs20ItCtvNvYTW+nLKOsgcd7tUy61Poi
yyCOCQvKCPG4lBF2xwr12vaJAuAfMUB72n6zX+9pqI9dobJxBUI0MwuHqnuKA4od
VZidw4F2BI1I1ITOa9gxCO0Q5k/LW7PF3aX/cUaUH7lovQC3vRTadtILAoGBAJm3
Hb+d+j+FLzBX0Ba8pqZpJ4Ftb7bZ86U9GG/hDXJBT6qHaANHAHT9qzU0h71z/So9
tNPtee94UuOIxWrq0TT0cect9t+7kTfYo/poMwdnj7Ix7V+S/EVSo1iBaSfPlC/h
/oiTZLxYpnDsOwrVJ0kTjAwYd9qzYo+XN7W/ZDUZAoGAMIFSVd953NSWD51K2I6a
hKbJ8rr5KosOcFpYtfBqfZkBQoqiWiS6GlQ03A7neG3CcW89/QroJpdLLhsMpyiH
jZFSlVsx6wYVBa5GqmwREps56x2VP7q+tvqRom8EOIYHruBWZDlPE2L2v5ETGw1Q
9NdVobq1K7NkX9L9a+gc9qA=
-----END PRIVATE KEY-----
`

// gorilla's defaults are 4096-byte read/write buffers — the same recv
// buffer size the Kāra demo's handle_connection allocates (Array[u8,
// 4096]). Stated explicitly so the buffer cost is part of the
// documented, comparable per-connection footprint, not a hidden default.
var upgrader = websocket.Upgrader{
	ReadBufferSize:  4096,
	WriteBufferSize: 4096,
	// Non-browser bench client; it sends no Origin header. Accept all
	// origins so the upgrade isn't rejected by gorilla's default
	// same-origin check.
	CheckOrigin: func(*http.Request) bool { return true },
}

// handleWS upgrades the request to a WebSocket and runs the
// echo-or-idle-hold loop, mirroring ../src/main.kara's handle_connection
// and the Rust comparator's per-task closure. Text/binary frames are
// echoed straight back; ping/pong/close control frames are handled by
// gorilla's defaults inside ReadMessage. An idle connection sends
// nothing, so this blocks in ReadMessage until the peer closes —
// density-neutral.
func handleWS(w http.ResponseWriter, r *http.Request) {
	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()
	for {
		mt, msg, err := conn.ReadMessage()
		if err != nil {
			return // peer close, read error, or protocol error
		}
		if mt == websocket.TextMessage || mt == websocket.BinaryMessage {
			if err := conn.WriteMessage(mt, msg); err != nil {
				return
			}
		}
	}
}

func main() {
	cert, err := tls.X509KeyPair([]byte(certPEM), []byte(keyPEM))
	if err != nil {
		log.Fatalf("load test cert/key: %v", err)
	}
	tlsConfig := &tls.Config{
		Certificates: []tls.Certificate{cert},
		MinVersion:   tls.VersionTLS12, // MaxVersion defaults to TLS 1.3
		// ClientAuth defaults to NoClientCert — matches rustls
		// with_no_client_auth.
	}

	// Bind the listener ourselves on 127.0.0.1:0 so we can read back the
	// ephemeral port and announce it before ServeTLS takes over the
	// accept loop — the BOUND_PORT convention the bench harness's
	// server-spawn path reads from stdout (same as the Kāra runtime's
	// karac_runtime_tcp_bind and the Rust comparator).
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		log.Fatalf("bind 127.0.0.1:0: %v", err)
	}
	port := ln.Addr().(*net.TCPAddr).Port

	fmt.Printf("BOUND_PORT=%d\n", port)
	if err := os.Stdout.Sync(); err != nil {
		// Best-effort flush; stdout is a pipe to the harness. A pipe
		// sync error (e.g. EINVAL on some platforms) is non-fatal —
		// Printf already wrote the line.
		_ = err
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/", handleWS)
	server := &http.Server{
		Handler:   mux,
		TLSConfig: tlsConfig,
		// Silence per-request handshake/upgrade errors. The "no
		// structured logging" omission, and it keeps a bad-handshake
		// storm from spamming the harness's stderr channel.
		ErrorLog: log.New(io.Discard, "", 0),
	}

	// Cert/key are already in server.TLSConfig, so the file args are
	// empty. ServeTLS wraps each accepted conn in TLS and dispatches to
	// the mux on its own goroutine. Only returns on a fatal accept-loop
	// error (e.g. the listener closing) — a single bad TLS handshake or
	// failed upgrade is per-request and does not stop the server, the
	// Go analogue of the Kāra accept loop's match-and-skip on Err.
	if err := server.ServeTLS(ln, "", ""); err != nil {
		log.Fatalf("serve: %v", err)
	}
}
