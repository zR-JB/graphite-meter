package goclient

// Version carries the git tag, stamped by release.yml alongside the sentinels
// in config.EngineVersion and client/package.json. "0.0.0-dev" marks a build
// that bypasses just/CI. Stamp it with
// -ldflags="-X github.com/zR-JB/graphite-meter/go/internal/goclient.Version=1.2.3".
var Version = "0.0.0-dev"
