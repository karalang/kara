// Slice E — Go reference impl for the three-language Parallax bench.
//
// `net/http` + four goroutines + `sync.WaitGroup` for fan-out. Same
// provider busy-loop kernel sizes as the Kāra impl so all four impls
// stay apples-to-apples.
//
// **Sleep substitute.** Per the F5 design lock, providers should
// approximate 2/5/8/12 ms latency. Kāra's stdlib has no `sleep_ms`
// so its impl uses CPU-bound busy loops; Go mirrors the busy-loop
// shape (not `time.Sleep`) at the same iteration counts. README
// footnotes the deviation.
//
// **Path parsing.** `r.URL.Path` is read but the user_id is hard-
// coded (the bench load is user_id-invariant since the busy loops
// dominate).
//
// **`BOUND_PORT=<n>` line.** Mirrors Kāra's runtime convention so
// `bench.sh` can use one port-discovery helper across all four impls.

package main

import (
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"sync"
)

const (
	fetchProfileWork    = 700_000
	fetchOrdersWork     = 4_000_000
	fetchNotifsWork     = 1_700_000
	fetchRecommendWork  = 2_700_000
)

func busyLoop(n int64) int64 {
	var sum int64 = 0
	for i := int64(0); i < n; i++ {
		sum += i
	}
	return sum
}

func fetchProfileName(userID int64) string {
	_ = busyLoop(fetchProfileWork) + userID
	return "Alice"
}

func fetchLatestOrderID(userID int64) int64 {
	_ = busyLoop(fetchOrdersWork) + userID
	return 1001
}

func fetchTopNotificationKind(userID int64) int64 {
	_ = busyLoop(fetchNotifsWork) + userID
	return 1
}

func fetchTopRecommendationID(userID int64) int64 {
	_ = busyLoop(fetchRecommendWork) + userID
	return 7001
}

type Profile struct {
	UserID int64  `json:"user_id"`
	Name   string `json:"name"`
}

type LatestOrder struct {
	OrderID int64 `json:"order_id"`
}

type TopNotification struct {
	Kind int64 `json:"kind"`
}

type TopRecommendation struct {
	ItemID int64 `json:"item_id"`
}

type Dashboard struct {
	Profile           Profile           `json:"profile"`
	LatestOrder       LatestOrder       `json:"latest_order"`
	TopNotification   TopNotification   `json:"top_notification"`
	TopRecommendation TopRecommendation `json:"top_recommendation"`
}

func getDashboard(userID int64) Dashboard {
	var (
		profileName string
		orderID     int64
		notifKind   int64
		recID       int64
	)
	var wg sync.WaitGroup
	wg.Add(4)
	go func() {
		defer wg.Done()
		profileName = fetchProfileName(userID)
	}()
	go func() {
		defer wg.Done()
		orderID = fetchLatestOrderID(userID)
	}()
	go func() {
		defer wg.Done()
		notifKind = fetchTopNotificationKind(userID)
	}()
	go func() {
		defer wg.Done()
		recID = fetchTopRecommendationID(userID)
	}()
	wg.Wait()
	return Dashboard{
		Profile:           Profile{UserID: userID, Name: profileName},
		LatestOrder:       LatestOrder{OrderID: orderID},
		TopNotification:   TopNotification{Kind: notifKind},
		TopRecommendation: TopRecommendation{ItemID: recID},
	}
}

func handle(w http.ResponseWriter, r *http.Request) {
	_ = r.URL.Path
	d := getDashboard(1)
	w.Header().Set("content-type", "application/json")
	w.WriteHeader(http.StatusOK)
	_ = json.NewEncoder(w).Encode(d)
}

func main() {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		fmt.Fprintln(nil, "bind failed:", err)
		return
	}
	addr := listener.Addr().(*net.TCPAddr)
	fmt.Printf("BOUND_PORT=%d\n", addr.Port)
	mux := http.NewServeMux()
	mux.HandleFunc("/", handle)
	_ = http.Serve(listener, mux)
}
