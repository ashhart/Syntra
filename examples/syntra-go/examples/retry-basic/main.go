// Copyright 2024 Ash Hart. Apache-2.0.

// retry-basic demonstrates the syntra-go retry client in minimal usage.
// Point SYNTRA_URL, SYNTRA_ADMIN_KEY, and SYNTRA_CAPSULE_PATH at a running
// Syntra instance (or the demo container at http://localhost:8787).
package main

import (
	"context"
	"fmt"
	"log"
	"net/http"
	"os"
	"time"

	syntra "github.com/ashhart/syntra-go"
	"github.com/ashhart/syntra-go/retry"
)

func main() {
	syntraURL := envOr("SYNTRA_URL", "http://localhost:8787")
	adminKey := envOr("SYNTRA_ADMIN_KEY", "dev-key")
	capsulePath := envOr("SYNTRA_CAPSULE_PATH", "/tenants/myteam/jobs/retry/capsules/router")
	targetURL := envOr("TARGET_URL", "https://httpbin.org/get")

	client := retry.NewRetryClient(retry.ClientOptions{
		SyntraOptions: syntra.ClientOptions{
			BaseURL:     syntraURL,
			AdminKey:    adminKey,
			CapsulePath: capsulePath,
			Timeout:     2 * time.Second,
		},
		FallbackPolicy: retry.PolicyByName(retry.PolicySingle),
		OnFeedbackError: func(err error) {
			log.Printf("feedback error (non-fatal): %v", err)
		},
	})

	req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, targetURL, nil)
	if err != nil {
		log.Fatalf("build request: %v", err)
	}

	resp, err := client.Do(req)
	if err != nil {
		log.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	fmt.Printf("response status: %s\n", resp.Status)
}

func envOr(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}
