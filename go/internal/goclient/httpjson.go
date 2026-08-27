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
	if err := json.UnmarshalRead(res.Body, out); err != nil {
		return nil, err
	}
	return res, nil
}

func httpStatusError(prefix string) func(*http.Response) error {
	return func(res *http.Response) error {
		return fmt.Errorf("%s returned HTTP %d", prefix, res.StatusCode)
	}
}
