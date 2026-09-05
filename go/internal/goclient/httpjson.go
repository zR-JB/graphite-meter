package goclient

import (
	"context"
	"encoding/json/v2"
	"fmt"
	"io"
	"net/http"
)

type jsonHTTPClient struct{ client *http.Client }

func (c jsonHTTPClient) requestJSON(ctx context.Context, method, target string, body io.Reader, headers http.Header, out any, statusError func(*http.Response) error) (*http.Response, error) {
	req, err := http.NewRequestWithContext(ctx, method, target, body)
	if err != nil {
		return nil, err
	}
	req.Header = headers.Clone()
	res, err := c.client.Do(req)
	if err != nil {
		return nil, err
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK {
		if err := authResponseError(res); err != nil {
			return nil, err
		}
		return nil, statusError(res)
	}
	if err := readControlJSON(res.Body, out); err != nil {
		return nil, err
	}
	return res, nil
}

func httpStatusError(prefix string) func(*http.Response) error {
	return func(res *http.Response) error {
		return fmt.Errorf("%s returned HTTP %d", prefix, res.StatusCode)
	}
}

// Control responses have a decoded-byte bound, including chunked or compressed bodies.
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
