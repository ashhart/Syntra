// Copyright 2024 Ash Hart. Apache-2.0.

// Package retry provides a Syntra-driven HTTP retry client.
//
// Each call to Do:
//  1. Asks Syntra for a retry policy via /decide, supplying per-host features
//     (recent_failure_rate, p99_latency_ms, hour).
//  2. Executes the real HTTP request with the chosen policy applied.
//  3. Records the outcome in the per-host rolling window.
//  4. Posts /feedback with success bit and latency penalty.
//
// Any Syntra error causes a silent fallback to the configured FallbackPolicy.
// Feedback errors are passed to OnFeedbackError (if set) and swallowed.
package retry

import (
	"context"
	"fmt"
	"math"
	"net/http"
	"net/url"
	"sync"
	"time"

	syntra "github.com/ashhart/syntra-go"
)

// Policy names match the demo capsule YAML options list.
const (
	PolicyNone            = "none"
	PolicySingle          = "single"
	PolicyTriple          = "triple"
	PolicyExponentialFast = "exponential_fast"
	PolicyExponentialSlow = "exponential_slow"
)

// RetryPolicy describes a concrete retry strategy.
type RetryPolicy struct {
	Name              string
	MaxRetries        int
	InitialBackoff    time.Duration
	BackoffMultiplier float64
}

// defaultPolicies are indexed to match the capsule's options[] list order.
var defaultPolicies = []*RetryPolicy{
	{Name: PolicyNone, MaxRetries: 0, InitialBackoff: 0, BackoffMultiplier: 1.0},
	{Name: PolicySingle, MaxRetries: 1, InitialBackoff: 0, BackoffMultiplier: 1.0},
	{Name: PolicyTriple, MaxRetries: 3, InitialBackoff: 0, BackoffMultiplier: 1.0},
	{Name: PolicyExponentialFast, MaxRetries: 3, InitialBackoff: 100 * time.Millisecond, BackoffMultiplier: 2.0},
	{Name: PolicyExponentialSlow, MaxRetries: 3, InitialBackoff: 500 * time.Millisecond, BackoffMultiplier: 2.0},
}

// PolicyFromIndex returns the policy at the given capsule option index, falling
// back to the first policy (none) when the index is out of range.
func PolicyFromIndex(idx int) *RetryPolicy {
	if idx >= 0 && idx < len(defaultPolicies) {
		return defaultPolicies[idx]
	}
	return defaultPolicies[0]
}

// PolicyByName returns the policy with the given name, or nil if unknown.
func PolicyByName(name string) *RetryPolicy {
	for _, p := range defaultPolicies {
		if p.Name == name {
			return p
		}
	}
	return nil
}

// outcome records the result of one HTTP attempt for the rolling window.
type outcome struct {
	success   bool
	latencyMs float64
}

// endpointTracker keeps a per-host rolling window of the last windowSize
// outcomes and computes feature vectors for Syntra.
type endpointTracker struct {
	mu       sync.Mutex
	window   int
	outcomes map[string][]outcome
}

func newEndpointTracker(window int) *endpointTracker {
	if window <= 0 {
		window = 100
	}
	return &endpointTracker{
		window:   window,
		outcomes: make(map[string][]outcome),
	}
}

// record appends an outcome for the host, trimming to the window size.
func (t *endpointTracker) record(host string, success bool, latencyMs float64) {
	t.mu.Lock()
	defer t.mu.Unlock()
	t.outcomes[host] = append(t.outcomes[host], outcome{success: success, latencyMs: latencyMs})
	if len(t.outcomes[host]) > t.window {
		t.outcomes[host] = t.outcomes[host][len(t.outcomes[host])-t.window:]
	}
}

// features returns the feature vector for a host. When the window is empty,
// conservative defaults are used (50 % failure rate, 1 s p99).
func (t *endpointTracker) features(host string) map[string]float64 {
	t.mu.Lock()
	outs := make([]outcome, len(t.outcomes[host]))
	copy(outs, t.outcomes[host])
	t.mu.Unlock()

	hour := math.Mod(float64(time.Now().Unix())/3600.0, 24.0)

	if len(outs) == 0 {
		return map[string]float64{
			"recent_failure_rate": 0.5,
			"p99_latency_ms":      1000.0,
			"hour":                hour,
		}
	}

	successes := 0
	lats := make([]float64, len(outs))
	for i, o := range outs {
		if o.success {
			successes++
		}
		lats[i] = o.latencyMs
	}
	failureRate := 1.0 - float64(successes)/float64(len(outs))

	// Sort latencies via simple insertion sort — window is at most 100 entries.
	for i := 1; i < len(lats); i++ {
		for j := i; j > 0 && lats[j] < lats[j-1]; j-- {
			lats[j], lats[j-1] = lats[j-1], lats[j]
		}
	}
	idx := len(lats)*99/100 - 1
	if idx < 0 {
		idx = 0
	}
	p99 := lats[idx]

	return map[string]float64{
		"recent_failure_rate": failureRate,
		"p99_latency_ms":      p99,
		"hour":                hour,
	}
}

// failureRate returns the rolling failure rate for a host (0–1). It is
// exported for tests and monitoring; the main path uses features().
func (t *endpointTracker) failureRate(host string) float64 {
	feats := t.features(host)
	return feats["recent_failure_rate"]
}

// Doer is the interface satisfied by *http.Client, enabling test injection.
type Doer interface {
	Do(req *http.Request) (*http.Response, error)
}

// ClientOptions configures a RetryClient.
type ClientOptions struct {
	// SyntraOptions configures the Syntra client (BaseURL, AdminKey, CapsulePath, Timeout).
	SyntraOptions syntra.ClientOptions

	// FallbackPolicy is used when Syntra is unreachable, refuses, or returns a
	// malformed decision. Defaults to "single".
	FallbackPolicy *RetryPolicy

	// HTTPClient is the underlying HTTP doer used to execute real requests.
	// Defaults to http.DefaultClient if nil.
	HTTPClient Doer

	// TrackerWindow is the rolling-window size for per-host stats. Default 100.
	TrackerWindow int

	// OnFeedbackError, when non-nil, is called with feedback errors instead of
	// discarding them silently. Must not block.
	OnFeedbackError func(error)

	// Sleep is the function used to implement backoff. Defaults to time.Sleep.
	// Override in tests for speed.
	Sleep func(time.Duration)
}

// RetryClient wraps an HTTP doer with Syntra-driven per-request retry policy
// selection. It is safe for concurrent use after construction.
type RetryClient struct {
	syntra          *syntra.Client
	fallback        *RetryPolicy
	doer            Doer
	tracker         *endpointTracker
	onFeedbackError func(error)
	sleep           func(time.Duration)
}

// NewRetryClient constructs a RetryClient from opts.
func NewRetryClient(opts ClientOptions) *RetryClient {
	fb := opts.FallbackPolicy
	if fb == nil {
		fb = defaultPolicies[1] // "single"
	}
	doer := opts.HTTPClient
	if doer == nil {
		doer = http.DefaultClient
	}
	slp := opts.Sleep
	if slp == nil {
		slp = time.Sleep
	}
	return &RetryClient{
		syntra:          syntra.NewClient(opts.SyntraOptions),
		fallback:        fb,
		doer:            doer,
		tracker:         newEndpointTracker(opts.TrackerWindow),
		onFeedbackError: opts.OnFeedbackError,
		sleep:           slp,
	}
}

// Do executes req with a Syntra-selected retry policy.
// It never returns a feedback error; those are passed to OnFeedbackError or
// silently dropped. Transport errors on the real request are returned only when
// all retry attempts are exhausted.
func (c *RetryClient) Do(req *http.Request) (*http.Response, error) {
	host := endpointHost(req.URL)
	features := c.tracker.features(host)

	policy, decisionID := c.getPolicy(req.Context(), features)

	resp, success, latencyMs, err := c.executeWithPolicy(req, policy)

	c.tracker.record(host, success, latencyMs)

	if decisionID != "" {
		c.sendFeedback(req.Context(), decisionID, success, latencyMs)
	}

	return resp, err
}

// endpointHost extracts the host (host[:port]) from a URL.
func endpointHost(u *url.URL) string {
	if u == nil {
		return "unknown"
	}
	if u.Host != "" {
		return u.Host
	}
	return u.String()
}

// getPolicy asks Syntra for a retry policy. On any error it returns the
// fallback policy and an empty decisionID.
func (c *RetryClient) getPolicy(ctx context.Context, features map[string]float64) (*RetryPolicy, string) {
	body := syntra.DecideBody{Features: features}
	decision, err := c.syntra.Decide(ctx, body)
	if err != nil {
		return c.fallback, ""
	}
	if decision.Refused || len(decision.Decisions) == 0 {
		return c.fallback, decision.DecisionID
	}
	policy := PolicyFromIndex(decision.Decisions[0].ChosenOption)
	return policy, decision.DecisionID
}

// executeWithPolicy runs req through the HTTP doer with the given retry policy.
// It returns the last response (may be non-nil even on error), a success
// indicator, total elapsed milliseconds, and the terminal error.
func (c *RetryClient) executeWithPolicy(req *http.Request, policy *RetryPolicy) (
	resp *http.Response, success bool, latencyMs float64, err error,
) {
	start := time.Now()
	backoff := policy.InitialBackoff

	for attempt := 0; attempt <= policy.MaxRetries; attempt++ {
		if resp != nil {
			// Drain and close the previous response before retrying.
			resp.Body.Close()
			resp = nil
		}

		var doErr error
		resp, doErr = c.doer.Do(req)

		if doErr == nil && resp.StatusCode < 500 {
			latencyMs = float64(time.Since(start).Milliseconds())
			success = resp.StatusCode < 400
			return resp, success, latencyMs, nil
		}

		// 5xx or transport error — maybe retry.
		err = doErr
		if attempt < policy.MaxRetries {
			if backoff > 0 {
				c.sleep(backoff)
				backoff = time.Duration(float64(backoff) * policy.BackoffMultiplier)
			}
		}
	}

	latencyMs = float64(time.Since(start).Milliseconds())
	success = false
	return resp, false, latencyMs, err
}

// sendFeedback posts reward to Syntra. Errors never surface to the caller.
func (c *RetryClient) sendFeedback(ctx context.Context, decisionID string, success bool, latencyMs float64) {
	latencyPenalty := math.Min(latencyMs/10000.0, 1.0)
	reward := 0.0
	if success {
		reward = 1.0
	}
	reward -= 0.3 * latencyPenalty

	err := c.syntra.Feedback(ctx, syntra.FeedbackBody{
		DecisionID: decisionID,
		Reward:     reward,
	})
	if err != nil {
		if c.onFeedbackError != nil {
			c.onFeedbackError(fmt.Errorf("syntra-retry: feedback: %w", err))
		}
		// Always swallowed — feedback failure must not break the caller.
	}
}

// Tracker exposes the per-host tracker for inspection and testing.
func (c *RetryClient) Tracker() interface {
	FailureRate(host string) float64
} {
	return trackerProxy{t: c.tracker}
}

type trackerProxy struct{ t *endpointTracker }

func (p trackerProxy) FailureRate(host string) float64 { return p.t.failureRate(host) }
