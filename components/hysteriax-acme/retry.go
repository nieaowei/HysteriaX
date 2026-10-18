package main

import (
	"net/http"
	"strconv"
	"sync"
	"time"

	"github.com/caddyserver/certmagic"
	"go.uber.org/zap/zapcore"
)

// CertMagic 0.25.4 does not expose error-response Retry-After headers. Its
// structured ACME transport diagnostics do. This sink observes only that header
// and status code, and deliberately writes/retains no diagnostic fields.
type retryObserver struct {
	mu    sync.Mutex
	after time.Time
}

func (r *retryObserver) Enabled(zapcore.Level) bool        { return true }
func (r *retryObserver) With([]zapcore.Field) zapcore.Core { return r }
func (r *retryObserver) Check(e zapcore.Entry, c *zapcore.CheckedEntry) *zapcore.CheckedEntry {
	return c.AddCore(e, r)
}
func (r *retryObserver) Sync() error { return nil }
func (r *retryObserver) Write(_ zapcore.Entry, fields []zapcore.Field) error {
	var h http.Header
	var status int64
	for _, f := range fields {
		if f.Key == "response_headers" {
			h, _ = f.Interface.(http.Header)
		}
		if f.Key == "status_code" {
			status = f.Integer
		}
	}
	if status < 400 || h == nil {
		return nil
	}
	after := retryAfter(h.Get("Retry-After"), time.Now())
	r.mu.Lock()
	defer r.mu.Unlock()
	if after.After(r.after) {
		r.after = after
	}
	return nil
}
func (r *retryObserver) deadline() time.Time { r.mu.Lock(); defer r.mu.Unlock(); return r.after }
func retryAfter(value string, now time.Time) time.Time {
	if seconds, e := strconv.ParseInt(value, 10, 32); e == nil && seconds >= 0 {
		return now.Add(time.Duration(seconds) * time.Second)
	}
	if date, e := http.ParseTime(value); e == nil {
		return date
	}
	return time.Time{}
}

type trackedIssuer struct {
	*certmagic.ACMEIssuer
	observer *retryObserver
}

func (i *trackedIssuer) retryDeadline() time.Time { return i.observer.deadline() }

// EAB happens before the ACME issuer exists; carry only its retry deadline.
type retryError struct{ after time.Time }

func (e retryError) Error() string            { return "EAB response rejected" }
func (e retryError) retryDeadline() time.Time { return e.after }
