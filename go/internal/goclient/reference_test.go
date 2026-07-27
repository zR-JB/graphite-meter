//go:build reference

// The native client's rate against a running server, as the browser
// benchmark's absolute reference. It sets socket buffers and an exact
// Content-Length, neither of which a browser can, so it is an upper bound on
// what any browser could reach rather than a target.
//
//	GM_REF_URL=http://127.0.0.1:7246 go test -tags reference \
//	  -run TestNativeReference -v -count=1 ./internal/goclient/
package goclient_test

import (
	"context"
	"os"
	"strconv"
	"testing"
	"time"

	"github.com/zR-JB/graphite-meter/go/internal/goclient"
)

func TestNativeReference(t *testing.T) {
	base := os.Getenv("GM_REF_URL")
	if base == "" {
		t.Skip("set GM_REF_URL to the server under test")
	}
	for _, lanes := range []int{1, 2, 4, 8} {
		for _, upload := range []bool{false, true} {
			dir := "download"
			if upload {
				dir = "upload"
			}
			t.Run(dir+"/lanes="+strconv.Itoa(lanes), func(t *testing.T) {
				cfg := goclient.DefaultConfig()
				cfg.BaseURL = base
				cfg.InsecureSkipTLSVerify = true
				cfg.Stages = goclient.StageSet{Download: !upload, Upload: upload}
				cfg.Warmup = 3 * time.Second
				cfg.DownloadDuration = 8 * time.Second
				cfg.UploadDuration = 8 * time.Second
				cfg.TransferStreams = goclient.TransferStreamPolicy{Forced: lanes}
				cfg.LoadedLatency = false

				ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
				defer cancel()
				var mean, peak float64
				if err := goclient.Run(ctx, cfg, func(e goclient.Event) {
					if e.Result != nil {
						mean = e.Result.MeanBps * 8 / 1e9
						peak = e.Result.PeakBps * 8 / 1e9
					}
				}); err != nil {
					t.Fatalf("run: %v", err)
				}
				t.Logf("%s lanes=%d mean %.2f peak %.2f Gbit/s", dir, lanes, mean, peak)
			})
		}
	}
}
