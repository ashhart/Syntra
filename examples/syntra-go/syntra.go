// Copyright 2024 Ash Hart. Apache-2.0.

// Package syntra provides a minimal Go client for the Syntra contextual-bandit
// appliance. It covers the /decide and /feedback HTTP endpoints with fail-safe
// semantics: any transport or protocol error is returned as a typed error rather
// than a panic, and feedback failures can be silently swallowed by providing an
// OnFeedbackError hook.
package syntra

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// ClientOptions configures a Syntra HTTP client.
type ClientOptions struct {
	// BaseURL is the Syntra appliance root, e.g. "http://localhost:8787".
	// Trailing slash is stripped automatically.
	BaseURL string

	// AdminKey is the Bearer token sent in every request.
	AdminKey string

	// CapsulePath is the path to the capsule, e.g.
	// "/tenants/myteam/jobs/retry/capsules/router".
	// Leading slash is required; trailing slash is stripped.
	CapsulePath string

	// Timeout is the per-request HTTP timeout. Defaults to 2 s.
	Timeout time.Duration

	// OnFeedbackError, when non-nil, is called instead of discarding feedback
	// errors silently. The hook must not block.
	OnFeedbackError func(error)
}

// DecisionItem represents one element of the decisions array returned by /decide.
type DecisionItem struct {
	ChosenOption int    `json:"chosen_option"`
	Label        string `json:"label,omitempty"`
}

// Decision is the parsed response from /decide.
type Decision struct {
	DecisionID string                 `json:"decisionId"`
	Decisions  []DecisionItem         `json:"decisions"`
	Refused    bool                   `json:"refused"`
	Confidence map[string]interface{} `json:"confidence"`
	OodScore   float64                `json:"oodScore"`
}

// DecideBody is the request body for /decide.
// Exactly one of ContextKey or Features should be set.
type DecideBody struct {
	// ContextKey is used for discrete-context capsules.
	ContextKey string `json:"contextKey,omitempty"`

	// Features is used for feature-context capsules.
	Features map[string]float64 `json:"features,omitempty"`
}

// FeedbackBody is the request body for /feedback.
type FeedbackBody struct {
	DecisionID string  `json:"decisionId"`
	Reward     float64 `json:"reward"`
}

// Client is a Syntra HTTP client. Create one with NewClient; it is safe for
// concurrent use once constructed.
type Client struct {
	opts       ClientOptions
	httpClient *http.Client
}

// NewClient constructs a Client from the supplied options.
// It does not make any network calls.
func NewClient(opts ClientOptions) *Client {
	opts.BaseURL = strings.TrimRight(opts.BaseURL, "/")
	opts.CapsulePath = strings.TrimRight(opts.CapsulePath, "/")
	if opts.Timeout <= 0 {
		opts.Timeout = 2 * time.Second
	}
	return &Client{
		opts: opts,
		httpClient: &http.Client{
			Timeout: opts.Timeout,
		},
	}
}

// Decide calls POST {capsulePath}/decide and returns the parsed Decision.
// A non-nil error indicates a transport or non-2xx response; the caller should
// apply a fallback policy in that case.
func (c *Client) Decide(ctx context.Context, body DecideBody) (*Decision, error) {
	payload, err := json.Marshal(body)
	if err != nil {
		return nil, fmt.Errorf("syntra: marshal decide body: %w", err)
	}

	url := c.opts.BaseURL + c.opts.CapsulePath + "/decide"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return nil, fmt.Errorf("syntra: build decide request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+c.opts.AdminKey)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("syntra: decide request: %w", err)
	}
	defer resp.Body.Close()

	rawBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("syntra: read decide response: %w", err)
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return nil, fmt.Errorf("syntra: decide status %d: %s", resp.StatusCode, rawBody)
	}

	var d Decision
	if err := json.Unmarshal(rawBody, &d); err != nil {
		return nil, fmt.Errorf("syntra: decode decide response: %w", err)
	}
	return &d, nil
}

// Feedback calls POST {capsulePath}/feedback.
// Errors are returned to the caller; RetryClient swallows them via the
// OnFeedbackError hook instead of propagating.
func (c *Client) Feedback(ctx context.Context, body FeedbackBody) error {
	payload, err := json.Marshal(body)
	if err != nil {
		return fmt.Errorf("syntra: marshal feedback body: %w", err)
	}

	url := c.opts.BaseURL + c.opts.CapsulePath + "/feedback"
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, url, bytes.NewReader(payload))
	if err != nil {
		return fmt.Errorf("syntra: build feedback request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+c.opts.AdminKey)

	resp, err := c.httpClient.Do(req)
	if err != nil {
		return fmt.Errorf("syntra: feedback request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		b, _ := io.ReadAll(resp.Body)
		return fmt.Errorf("syntra: feedback status %d: %s", resp.StatusCode, b)
	}
	return nil
}
