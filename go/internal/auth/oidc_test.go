package auth

import (
	"errors"
	"net/http"
	"testing"
)

func TestProviderClientRejectsRedirects(t *testing.T) {
	client := providerHTTPClient()
	req, _ := http.NewRequest(http.MethodGet, "http://provider.example/token", nil)
	if err := client.CheckRedirect(req, nil); !errors.Is(err, http.ErrUseLastResponse) {
		t.Fatalf("redirect error=%v", err)
	}
}
