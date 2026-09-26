package goclient

import (
	"context"
	"encoding/json/v2"
	"fmt"
	"io"
	"net/http"
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
		if err := authResponseError(res); err != nil {
			return nil, err
		}
		return nil, fmt.Errorf("%s returned HTTP %d", what, res.StatusCode)
	}
	return res, readControlJSON(res.Body, out)
}

func unexpectedStatus(res *http.Response) error {
	if err := authResponseError(res); err != nil {
		return err
	}
	return fmt.Errorf("HTTP %d from %s", res.StatusCode, res.Request.URL.Redacted())
}

const maxControlBytes = 64 * 1024

func readControlJSON(body io.Reader, out any) error {
	data, err := io.ReadAll(io.LimitReader(body, maxControlBytes+1))
	if err != nil {
		return err
	}
	if len(data) > maxControlBytes {
		return fmt.Errorf("control response exceeds %d bytes", maxControlBytes)
	}
	return json.Unmarshal(data, out)
}
