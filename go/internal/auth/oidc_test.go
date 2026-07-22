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

func TestProviderHealthStatusPolicy(t *testing.T) {
	for _, status := range []int{http.StatusOK, http.StatusNoContent, http.StatusBadRequest, http.StatusUnauthorized, http.StatusForbidden, http.StatusMethodNotAllowed} {
		if !healthyProviderStatus(status, false) {
			t.Errorf("status %d rejected", status)
		}
	}
	for _, status := range []int{http.StatusFound, http.StatusNotFound, http.StatusRequestTimeout, http.StatusGone, http.StatusTooManyRequests, http.StatusInternalServerError} {
		if healthyProviderStatus(status, false) {
			t.Errorf("status %d accepted", status)
		}
	}
	if !healthyProviderStatus(http.StatusOK, true) || healthyProviderStatus(http.StatusNoContent, true) || healthyProviderStatus(http.StatusUnauthorized, true) {
		t.Fatal("JWKS health policy must require HTTP 200")
	}
}
