// Command authpreview serves the production login page on loopback with sample
// state, so its layout can be checked without a running identity provider.
package main

import (
	"flag"
	"fmt"
	"log"
	"net/http"

	"github.com/zR-JB/graphite-meter/go/internal/auth"
)

func main() {
	mode := flag.String("mode", "hybrid", "login mode: password, oidc, or hybrid")
	oidcReady := flag.Bool("oidc-ready", true, "show the identity provider as available")
	flag.Parse()
	if *mode != "password" && *mode != "oidc" && *mode != "hybrid" {
		log.Fatal("mode must be password, oidc, or hybrid")
	}
	const address = "127.0.0.1:4174"
	fmt.Printf("Login preview: http://%s/login\n", address)
	log.Fatal(http.ListenAndServe(address, auth.PreviewHandler(*mode, *oidcReady)))
}
