// Go reference impl ("comparator") for the `ws_idle_holder` flagship
// demo — gorilla/websocket + crypto/tls, mirroring `../src/main.kara`.
// Standalone module, NOT part of the karac-rust Cargo workspace (the
// language is different); mirrors the sibling `../rust` comparator and
// the `../bench` harness in being self-rooted.
//
// `go 1.21` is the language floor (gorilla/websocket needs only 1.12+);
// the rig installs a current Go toolchain via `../bench/scripts/ec2_setup.sh`.
module ws-idle-holder-go

go 1.21

require github.com/gorilla/websocket v1.5.3
