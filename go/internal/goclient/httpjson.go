package goclient

import (
	"context"
	"encoding/json/v2"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"time"
)

func controlJSON(
	ctx context.Context,
	client *http.Client,
	method, target, what string,
	out any,
) (*http.Response, error) {
	req, err := http.NewRequestWithContext(ctx, method, target, nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("Cache-Control", "no-store")
	res, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		return nil, statusOf(res, what)
	}
	return res, readControlJSON(res.Body, out)
}

func unexpectedStatus(res *http.Response) error { return statusOf(res, res.Request.URL.Path) }

// statusOf takes the source from the caller: dial responses carry no Request.
func statusOf(res *http.Response, from string) error {
	if err := authResponseError(res); err != nil {
		return err
	}
	// Delta-seconds only; 32 bits keep the duration from overflowing.
	seconds, _ := strconv.ParseUint(res.Header.Get("Retry-After"), 10, 32)
	status := statusError{res.StatusCode, from, time.Duration(seconds) * time.Second}
	if code := res.Header.Get("X-Graphite-Upload-Refusal"); code != "" {
		return uploadRefusal(code, status)
	}
	return status
}

const maxControlBytes = 64 * 1024

func readControlJSON(body io.Reader, out any) error {
	data, err := io.ReadAll(io.LimitReader(body, maxControlBytes+1))
	if err != nil {
		return err
	}
	if len(data) > maxControlBytes {
		return fmt.Errorf("%w: control response exceeds %d bytes", errProtocol, maxControlBytes)
	}
	if err := json.Unmarshal(data, out); err != nil {
		return fmt.Errorf("%w: %w", errProtocol, err)
	}
	return nil
}
