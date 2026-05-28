// Copyright 2024 Ash Hart. Apache-2.0.

package retry

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	syntra "github.com/ashhart/syntra-go"
)

// --- helpers -----------------------------------------------------------------

// syntraDecideResponse returns a minimal /decide JSON body.
func syntraDecideResponse(decisionID string, chosenOption int, refused bool) []byte {
	resp := map[string]interface{}{
		"decisionId": decisionID,
		"refused":    refused,
		"decisions":  []map[string]interface{}{{"chosen_option": chosenOption}},
		"confidence": nil,
		"oodScore":   0.0,
	}
	if refused {
		resp["decisions"] = []map[string]interface{}{}
	}
	b, _ := json.Marshal(resp)
	return b
}

// mockSyntra creates a test server that responds to /decide and /feedback.
// decideFunc receives the raw request body and returns (status, body).
// feedbackFunc is called for each /feedback request.
func mockSyntra(
	t *testing.T,
	decideFunc func(body []byte) (int, []byte),
	feedbackFunc func(body []byte),
) *httptest.Server {
	t.Helper()
	return httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		var buf []byte
		if r.Body != nil {
			buf = make([]byte, 0, 512)
			tmp := make([]byte, 512)
			for {
				n, _ := r.Body.Read(tmp)
				if n == 0 {
					break
				}
				buf = append(buf, tmp[:n]...)
			}
		}
		if strings.HasSuffix(r.URL.Path, "/decide") {
			status, body := decideFunc(buf)
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(status)
			_, _ = w.Write(body)
			return
		}
		if strings.HasSuffix(r.URL.Path, "/feedback") {
			if feedbackFunc != nil {
				feedbackFunc(buf)
			}
			w.WriteHeader(http.StatusOK)
			return
		}
		w.WriteHeader(http.StatusNotFound)
	}))
}

// newRetryClient builds a RetryClient wired to the given syntraURL and backend doer.
func newRetryClient(
	t *testing.T,
	syntraServer *httptest.Server,
	doer Doer,
	opts ...func(*ClientOptions),
) *RetryClient {
	t.Helper()
	o := ClientOptions{
		SyntraOptions: syntra.ClientOptions{
			BaseURL:     syntraServer.URL,
			AdminKey:    "test-key",
			CapsulePath: "/tenants/t/jobs/j/capsules/c",
			Timeout:     2 * time.Second,
		},
		HTTPClient: doer,
		// Override sleep so backoff tests don't actually wait.
		Sleep: func(d time.Duration) {},
	}
	for _, fn := range opts {
		fn(&o)
	}
	return NewRetryClient(o)
}

// staticDoer returns a fixed status code for every request.
type staticDoer struct {
	status int
	calls  atomic.Int64
}

func (d *staticDoer) Do(req *http.Request) (*http.Response, error) {
	d.calls.Add(1)
	rec := httptest.NewRecorder()
	rec.WriteHeader(d.status)
	return rec.Result(), nil
}

// errorDoer always returns a transport error.
type errorDoer struct {
	calls atomic.Int64
}

func (d *errorDoer) Do(req *http.Request) (*http.Response, error) {
	d.calls.Add(1)
	return nil, fmt.Errorf("connection refused")
}

// --- tests -------------------------------------------------------------------

// Test 1: Successful decide + feedback round-trip.
func TestSuccessfulRoundTrip(t *testing.T) {
	var feedbackCalled atomic.Bool
	syntraServer := mockSyntra(t,
		func(_ []byte) (int, []byte) {
			return http.StatusOK, syntraDecideResponse("dec-001", 1, false)
		},
		func(body []byte) {
			feedbackCalled.Store(true)
			var fb syntra.FeedbackBody
			if err := json.Unmarshal(body, &fb); err != nil {
				t.Errorf("feedback unmarshal: %v", err)
				return
			}
			if fb.DecisionID != "dec-001" {
				t.Errorf("feedback decisionId = %q, want dec-001", fb.DecisionID)
			}
		},
	)
	defer syntraServer.Close()

	backend := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
	}))
	defer backend.Close()

	client := newRetryClient(t, syntraServer, http.DefaultClient)
	req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, backend.URL+"/test", nil)
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Errorf("status = %d, want 200", resp.StatusCode)
	}
	if !feedbackCalled.Load() {
		t.Error("feedback was not sent")
	}
}

// Test 2: Refusal falls back to default policy.
func TestRefusalFallsBackToDefault(t *testing.T) {
	syntraServer := mockSyntra(t,
		func(_ []byte) (int, []byte) {
			// refused = true, no decisions
			return http.StatusOK, syntraDecideResponse("dec-002", 0, true)
		},
		nil,
	)
	defer syntraServer.Close()

	doer := &staticDoer{status: http.StatusOK}
	client := newRetryClient(t, syntraServer, doer, func(o *ClientOptions) {
		o.FallbackPolicy = defaultPolicies[0] // "none"
	})

	req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "http://example.com/test", nil)
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("Do: %v", err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Errorf("status = %d, want 200", resp.StatusCode)
	}
	// With "none" policy, exactly 1 attempt.
	if got := doer.calls.Load(); got != 1 {
		t.Errorf("doer called %d times, want 1", got)
	}
}

// Test 3: Syntra unreachable → fallback used, no panic.
func TestSyntraUnreachableFallback(t *testing.T) {
	// Point at a server that is immediately closed.
	syntraServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {}))
	syntraServer.Close() // closed right away

	doer := &staticDoer{status: http.StatusOK}
	client := NewRetryClient(ClientOptions{
		SyntraOptions: syntra.ClientOptions{
			BaseURL:     syntraServer.URL,
			AdminKey:    "key",
			CapsulePath: "/tenants/t/jobs/j/capsules/c",
			Timeout:     100 * time.Millisecond,
		},
		HTTPClient:    doer,
		FallbackPolicy: defaultPolicies[1], // "single"
		Sleep:          func(d time.Duration) {},
	})

	req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "http://example.com/", nil)
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("Do must not return an error when backend succeeds: %v", err)
	}
	if resp.StatusCode != http.StatusOK {
		t.Errorf("status = %d, want 200", resp.StatusCode)
	}
}

// Test 4: Feedback failure does not propagate as an error.
func TestFeedbackFailureSwallowed(t *testing.T) {
	var feedbackErr atomic.Value
	syntraServer := mockSyntra(t,
		func(_ []byte) (int, []byte) {
			return http.StatusOK, syntraDecideResponse("dec-004", 0, false)
		},
		func(_ []byte) {
			// Trigger 500 from feedback by not responding here — actually we
			// need to make /feedback 500. We do that via a custom handler below.
		},
	)
	defer syntraServer.Close()

	// Build a custom Syntra server that returns 500 for feedback.
	feedbackSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if strings.HasSuffix(r.URL.Path, "/decide") {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusOK)
			_, _ = w.Write(syntraDecideResponse("dec-004", 0, false))
			return
		}
		// /feedback always fails
		w.WriteHeader(http.StatusInternalServerError)
	}))
	defer feedbackSrv.Close()
	_ = syntraServer // unused below; feedbackSrv takes over

	doer := &staticDoer{status: http.StatusOK}
	client := NewRetryClient(ClientOptions{
		SyntraOptions: syntra.ClientOptions{
			BaseURL:     feedbackSrv.URL,
			AdminKey:    "key",
			CapsulePath: "/tenants/t/jobs/j/capsules/c",
			Timeout:     2 * time.Second,
		},
		HTTPClient: doer,
		Sleep:      func(d time.Duration) {},
		OnFeedbackError: func(err error) {
			feedbackErr.Store(err)
		},
	})

	req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "http://example.com/", nil)
	_, err := client.Do(req)
	if err != nil {
		t.Fatalf("Do must not return feedback error: %v", err)
	}
	// OnFeedbackError should have been called.
	if feedbackErr.Load() == nil {
		t.Error("expected OnFeedbackError to be called, but it was not")
	}
}

// Test 5: Per-host tracker accumulates outcomes and computes failure rate.
func TestTrackerAccumulatesFailureRate(t *testing.T) {
	tracker := newEndpointTracker(10)
	// 4 failures, 6 successes → 40 % failure rate.
	for i := 0; i < 6; i++ {
		tracker.record("api.example.com", true, 100)
	}
	for i := 0; i < 4; i++ {
		tracker.record("api.example.com", false, 200)
	}
	got := tracker.failureRate("api.example.com")
	want := 0.4
	if math.Abs(got-want) > 1e-9 {
		t.Errorf("failureRate = %.4f, want %.4f", got, want)
	}
}

// Test 6: Retry policy executes the correct number of attempts.
func TestRetryPolicyAttemptCount(t *testing.T) {
	tests := []struct {
		policyIdx     int
		wantAttempts  int64
	}{
		{0, 1}, // none: 0 retries → 1 attempt
		{1, 2}, // single: 1 retry → 2 attempts
		{2, 4}, // triple: 3 retries → 4 attempts
	}
	for _, tc := range tests {
		tc := tc
		t.Run(defaultPolicies[tc.policyIdx].Name, func(t *testing.T) {
			syntraServer := mockSyntra(t,
				func(_ []byte) (int, []byte) {
					return http.StatusOK, syntraDecideResponse("dec-6", tc.policyIdx, false)
				},
				nil,
			)
			defer syntraServer.Close()

			doer := &staticDoer{status: http.StatusServiceUnavailable}
			client := newRetryClient(t, syntraServer, doer)

			req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "http://example.com/", nil)
			_, _ = client.Do(req)

			if got := doer.calls.Load(); got != tc.wantAttempts {
				t.Errorf("policy %s: attempts = %d, want %d", defaultPolicies[tc.policyIdx].Name, got, tc.wantAttempts)
			}
		})
	}
}

// Test 7: Backoff respects the multiplier sequence.
func TestBackoffMultiplier(t *testing.T) {
	syntraServer := mockSyntra(t,
		func(_ []byte) (int, []byte) {
			// exponential_fast: idx 3, MaxRetries=3, InitialBackoff=100ms, mult=2
			return http.StatusOK, syntraDecideResponse("dec-7", 3, false)
		},
		nil,
	)
	defer syntraServer.Close()

	var sleptDurations []time.Duration
	var mu sync.Mutex // protect slice

	doer := &staticDoer{status: http.StatusServiceUnavailable}
	client := newRetryClient(t, syntraServer, doer, func(o *ClientOptions) {
		o.Sleep = func(d time.Duration) {
			mu.Lock()
			sleptDurations = append(sleptDurations, d)
			mu.Unlock()
		}
	})

	req, _ := http.NewRequestWithContext(context.Background(), http.MethodGet, "http://example.com/", nil)
	_, _ = client.Do(req)

	mu.Lock()
	got := sleptDurations
	mu.Unlock()

	// exponential_fast: 3 retries → 3 sleeps: 100ms, 200ms, 400ms.
	want := []time.Duration{100 * time.Millisecond, 200 * time.Millisecond, 400 * time.Millisecond}
	if len(got) != len(want) {
		t.Fatalf("sleep calls = %d, want %d; got %v", len(got), len(want), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Errorf("sleep[%d] = %v, want %v", i, got[i], want[i])
		}
	}
}
