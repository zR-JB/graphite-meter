package goclient

// Version is overridable at build time via
//
//	-ldflags="-X github.com/zR-JB/graphite-meter/go/internal/goclient.Version=1.2.3"
//
// Frozen "0.0.0-dev" sentinel for unstamped builds (raw `go build`, bypassing
// just/CI) — never bumped by hand. Real versions come from the git tag via
// release.yml; see go/internal/config.EngineVersion (server) and
// client/package.json's "version" (web client) for the equivalent sentinels.
var Version = "0.0.0-dev"
