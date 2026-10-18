package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/json"
	"encoding/pem"
	"errors"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/caddyserver/certmagic"
	"github.com/mholt/acmez/v3/acme"
	"go.uber.org/zap"
)

var isolatedRoots = x509.NewCertPool()

func config(t *testing.T) Config {
	t.Helper()
	root := t.TempDir()
	return Config{trustRoots: isolatedRoots, PolicyID: strings.Repeat("a", 64), StateDir: filepath.Join(root, "private", strings.Repeat("a", 64)), PublishDir: filepath.Join(root, "public"), ACME: Options{Domains: []string{"a.example", "b.example"}, Dir: filepath.Join(root, "storage")}}
}
func sign(t *testing.T, csr *x509.CertificateRequest, now time.Time) []byte {
	t.Helper()
	key, e := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if e != nil {
		t.Fatal(e)
	}
	ca := &x509.Certificate{SerialNumber: big.NewInt(1), Subject: pkix.Name{CommonName: "isolated test CA"}, NotBefore: now.Add(-time.Hour), NotAfter: now.Add(365 * 24 * time.Hour), IsCA: true, BasicConstraintsValid: true, KeyUsage: x509.KeyUsageCertSign}
	root, e := x509.CreateCertificate(rand.Reader, ca, ca, key.Public(), key)
	if e != nil {
		t.Fatal(e)
	}
	rootCert, _ := x509.ParseCertificate(root)
	isolatedRoots.AddCert(rootCert)
	leaf := &x509.Certificate{SerialNumber: big.NewInt(time.Now().UnixNano()), DNSNames: csr.DNSNames, NotBefore: now.Add(-time.Minute), NotAfter: now.Add(90 * 24 * time.Hour), KeyUsage: x509.KeyUsageDigitalSignature, ExtKeyUsage: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}
	der, e := x509.CreateCertificate(rand.Reader, leaf, ca, csr.PublicKey, key)
	if e != nil {
		t.Fatal(e)
	}
	return append(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: der}), pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: root})...)
}
func pair(t *testing.T, names []string, now time.Time) ([]byte, []byte) {
	csr, key, e := newCSR(names)
	if e != nil {
		t.Fatal(e)
	}
	return sign(t, csr, now), key
}

type fakeIssuer struct {
	t     *testing.T
	calls int
	fail  error
	wait  time.Time
}

func (f *fakeIssuer) PreCheck(context.Context, []string, bool) error { return nil }
func (f *fakeIssuer) Issue(_ context.Context, csr *x509.CertificateRequest) (*certmagic.IssuedCertificate, error) {
	f.calls++
	if f.fail != nil {
		return nil, f.fail
	}
	if len(csr.DNSNames) != 2 || csr.CheckSignature() != nil {
		f.t.Fatal("invalid SAN CSR")
	}
	return &certmagic.IssuedCertificate{Certificate: sign(f.t, csr, time.Now())}, nil
}
func (f *fakeIssuer) retryDeadline() time.Time { return f.wait }
func TestLifecycleRetryPublicationAndIsolation(t *testing.T) {
	c := config(t)
	if e := c.ensureDirs(); e != nil {
		t.Fatal(e)
	}
	fake := &fakeIssuer{t: t, fail: errors.New("credential=secret-token"), wait: time.Now().Add(12 * time.Hour)}
	create := func(context.Context) (issuer, error) { return fake, nil }
	if e := c.maintain(context.Background(), false, create); e == nil || strings.Contains(e.Error(), "secret-token") {
		t.Fatal(e)
	}
	s := c.status()
	if s.Failures != 1 || s.NextAttempt == nil || s.NextAttempt.Before(fake.wait) || s.State != "failed" {
		t.Fatalf("bad persistent retry: %+v", s)
	}
	if e := c.maintain(context.Background(), false, create); e == nil || fake.calls != 1 {
		t.Fatal("restart ignored retry")
	}
	s.NextAttempt = nil
	c.saveStatus(s)
	fake.fail = nil
	if e := c.maintain(context.Background(), false, create); e != nil {
		t.Fatal(e)
	}
	s = c.status()
	if s.State != "ready" || s.Failures != 0 || s.NextAttempt != nil || s.LastSuccess == nil {
		t.Fatalf("bad success: %+v", s)
	}
	old, _ := os.Readlink(filepath.Join(c.PublishDir, "current"))
	cert, key := pair(t, []string{"wrong.example"}, time.Now())
	if _, e := c.publish(cert, key); e == nil {
		t.Fatal("accepted incomplete SAN")
	}
	current, _ := os.Readlink(filepath.Join(c.PublishDir, "current"))
	if old != current {
		t.Fatal("failed publication changed current")
	}
	if e := c.maintain(context.Background(), true, create); e != nil || fake.calls != 2 {
		t.Fatal("ensure did not reuse valid certificate", e)
	}
	info, _ := os.Stat(filepath.Join(c.StateDir, "status.json"))
	if info.Mode().Perm() != 0600 {
		t.Fatal("private status permissions")
	}
	leaf, e := c.current()
	if e != nil {
		t.Fatal(e)
	}
	if renewalTime(leaf) != leaf.NotAfter.Add(-leaf.NotAfter.Sub(leaf.NotBefore)/3) {
		t.Fatal("renewal window")
	}
}
func TestRecoveryLockAndCertificateValidation(t *testing.T) {
	c := config(t)
	c.ensureDirs()
	f, e := c.lock()
	if e != nil {
		t.Fatal(e)
	}
	if _, e = c.lock(); e == nil {
		t.Fatal("concurrent writer acquired lock")
	}
	f.Close()
	cert, key := pair(t, c.ACME.Domains, time.Now())
	leaf, e := c.publish(cert, key)
	if e != nil {
		t.Fatal(e)
	}
	old, _ := os.Readlink(filepath.Join(c.PublishDir, "current"))
	os.Remove(filepath.Join(c.PublishDir, "current"))
	name := filepath.Base(old)
	if e = writeJSON(filepath.Join(c.StateDir, "publish.json"), name, 0600); e != nil {
		t.Fatal(e)
	}
	if e = c.recover(); e != nil {
		t.Fatal(e)
	}
	if current, _ := os.Readlink(filepath.Join(c.PublishDir, "current")); current != old {
		t.Fatal("did not recover durable intent")
	}
	if _, e = validatePair(cert, key, c.ACME.Domains, leaf.NotAfter.Add(time.Second)); e == nil {
		t.Fatal("accepted expiry")
	}
	_, wrong := pair(t, c.ACME.Domains, time.Now())
	if _, e = validatePair(cert, wrong, c.ACME.Domains, time.Now()); e == nil {
		t.Fatal("accepted mismatched key")
	}
	block, _ := pem.Decode(cert)
	if _, e = validatePair(pem.EncodeToMemory(block), key, c.ACME.Domains, time.Now()); e == nil {
		t.Fatal("accepted missing chain")
	}
	for i := 1; i < 30; i++ {
		if d := backoff(i); d < time.Minute || d > 6*time.Hour {
			t.Fatal("invalid backoff", d)
		}
	}
}
func TestMigrationReusesOnlyCompleteSAN(t *testing.T) {
	c := config(t)
	c.ensureDirs()
	dir := filepath.Join(c.storageDir(), "certificates", "ca", "a.example")
	os.MkdirAll(dir, 0700)
	cert, key := pair(t, c.ACME.Domains, time.Now())
	os.WriteFile(filepath.Join(dir, "a.example.crt"), cert, 0600)
	os.WriteFile(filepath.Join(dir, "a.example.key"), key, 0600)
	if e := c.maintain(context.Background(), true, func(context.Context) (issuer, error) { t.Fatal("valid legacy SAN reissued"); return nil, nil }); e != nil {
		t.Fatal(e)
	}
}
func TestProviderFixturesAndChallengeMapping(t *testing.T) {
	var fixtures []struct {
		Provider string            `json:"provider"`
		Config   map[string]string `json:"config"`
		Required []string          `json:"required"`
	}
	b, e := os.ReadFile("../../tests/fixtures/acme-dns-providers.json")
	if e != nil {
		t.Fatal(e)
	}
	if e = json.Unmarshal(b, &fixtures); e != nil {
		t.Fatal(e)
	}
	for _, f := range fixtures {
		t.Run(f.Provider, func(t *testing.T) {
			c := config(t)
			c.ACME.Type = "dns"
			c.ACME.DNS = &DNS{Name: f.Provider, Config: f.Config}
			f.Config["future_parameter"] = "preserved"
			if e := c.check(); e != nil {
				t.Fatal(e)
			}
			p, e := provider(c.ACME.DNS)
			if e != nil {
				t.Fatal(e)
			}
			value := reflect.ValueOf(p).Elem()
			seen := map[string]bool{}
			for i := 0; i < value.NumField(); i++ {
				if value.Field(i).Kind() == reflect.String {
					seen[value.Field(i).String()] = true
				}
			}
			for key, value := range f.Config {
				if key != "future_parameter" && !seen[value] {
					t.Fatalf("parameter %s not mapped", key)
				}
			}
			iss, e := c.issuer(context.Background())
			if e != nil {
				t.Fatal(e)
			}
			if iss.DNS01Solver == nil || !iss.DisableHTTPChallenge || !iss.DisableTLSALPNChallenge {
				t.Fatal("wrong DNS challenge")
			}
			for _, key := range f.Required {
				old := f.Config[key]
				f.Config[key] = ""
				if _, e := provider(c.ACME.DNS); e == nil {
					t.Fatal("accepted missing credential", key)
				}
				f.Config[key] = old
			}
		})
	}
	for _, kind := range []string{"http", "tls", ""} {
		c := config(t)
		c.ACME.Type = kind
		c.ACME.ListenHost = "127.0.0.1"
		c.ACME.Email = "ops@example.com"
		switch kind {
		case "http":
			c.ACME.HTTP = &Challenge{AltPort: 8080}
		case "tls":
			c.ACME.TLS = &Challenge{AltPort: 8443}
		default:
			c.ACME.DisableTLSALPN = true
			c.ACME.AltHTTPPort = 8080
		}
		if e := c.check(); e != nil {
			t.Fatal(e)
		}
		i, e := c.issuer(context.Background())
		if e != nil {
			t.Fatal(e)
		}
		if i.ListenHost != "127.0.0.1" || i.Email != "ops@example.com" || i.CA != certmagic.LetsEncryptProductionCA || i.TestCA != "" {
			t.Fatal("common mapping lost")
		}
		if kind == "tls" {
			if !i.DisableHTTPChallenge || i.DisableTLSALPNChallenge || i.AltTLSALPNPort != 8443 {
				t.Fatal("TLS mapping")
			}
		} else if i.DisableHTTPChallenge || !i.DisableTLSALPNChallenge || i.AltHTTPPort != 8080 {
			t.Fatal("HTTP mapping")
		}
	}
	for _, h := range []bool{false, true} {
		for _, tls := range []bool{false, true} {
			c := config(t)
			c.ACME.DisableHTTP = h
			c.ACME.DisableTLSALPN = tls
			if e := c.check(); (e != nil) != (h && tls) {
				t.Fatal("legacy validation")
			}
		}
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }
func TestZeroSSLEABAndRetryAfterNeverLogSecrets(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		r.ParseForm()
		if r.Form.Get("email") != "test@example.com" {
			t.Error("email lost")
		}
		w.Write([]byte(`{"success":true,"eab_kid":"private-id","eab_hmac_key":"private-mac"}`))
	}))
	defer server.Close()
	client := &http.Client{Transport: roundTripFunc(func(r *http.Request) (*http.Response, error) {
		u := *r.URL
		target, _ := http.NewRequest("POST", server.URL, r.Body)
		target.Header = r.Header
		r.URL = target.URL
		defer func() { r.URL = &u }()
		return http.DefaultTransport.RoundTrip(r)
	})}
	eab, e := zeroSSLEAB(context.Background(), "test@example.com", client)
	if e != nil || eab != (acme.EAB{KeyID: "private-id", MACKey: "private-mac"}) {
		t.Fatal(e, eab)
	}
	c := config(t)
	c.ensureDirs()
	c.ACME.CA = "zero"
	writeJSON(filepath.Join(c.StateDir, "eab.json"), eab, 0600)
	iss, e := c.issuer(context.Background())
	if e != nil || iss.CA != certmagic.ZeroSSLProductionCA || *iss.ExternalAccount != eab {
		t.Fatal("cached EAB mapping", e)
	}
	observer := &retryObserver{}
	log := zap.New(observer)
	now := time.Now()
	log.Debug("http request", zap.Any("headers", http.Header{"Authorization": {"secret"}}), zap.Any("response_headers", http.Header{"Retry-After": {"43200"}}), zap.Int("status_code", 429))
	if observer.deadline().Before(now.Add(12 * time.Hour)) {
		t.Fatal("CA retry-after lost")
	}
	date := time.Now().Add(24 * time.Hour).UTC().Truncate(time.Second)
	if !retryAfter(date.Format(http.TimeFormat), now).Equal(date) {
		t.Fatal("HTTP-date retry-after lost")
	}
	if !retryAfter("invalid", now).IsZero() {
		t.Fatal("invalid retry-after accepted")
	}
}

func TestChallengePortConflictAndCancelledIssuance(t *testing.T) {
	c := config(t)
	c.ensureDirs()
	listener, e := net.Listen("tcp", "127.0.0.1:0")
	if e != nil {
		t.Fatal(e)
	}
	defer listener.Close()
	c.ACME.Type = "http"
	c.ACME.ListenHost = "127.0.0.1"
	c.ACME.HTTP = &Challenge{AltPort: listener.Addr().(*net.TCPAddr).Port}
	if e = c.maintain(context.Background(), false, func(context.Context) (issuer, error) {
		t.Fatal("CA contacted despite busy challenge port")
		return nil, nil
	}); e == nil || !strings.Contains(e.Error(), "port conflicts") {
		t.Fatal(e)
	}
	c.ACME.Type = ""
	c.ACME.HTTP = nil
	status := c.status()
	status.NextAttempt = nil
	c.saveStatus(status)
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if e = c.maintain(ctx, false, func(ctx context.Context) (issuer, error) { return nil, ctx.Err() }); e == nil {
		t.Fatal("cancelled issuance succeeded")
	}
	status = c.status()
	if status.NextAttempt == nil || status.Failures != 2 {
		t.Fatal("cancelled attempt lost retry state")
	}
}

func TestScheduledRenewalRotatesKeyAndKeepsOldPublicationOnFailure(t *testing.T) {
	c := config(t)
	c.ensureDirs()
	cert, key := pair(t, c.ACME.Domains, time.Now().Add(-70*24*time.Hour))
	old, e := c.publish(cert, key)
	if e != nil {
		t.Fatal(e)
	}
	fake := &fakeIssuer{t: t, fail: errors.New("upstream provider secret")}
	create := func(context.Context) (issuer, error) { return fake, nil }
	if e = c.maintain(context.Background(), false, create); e == nil {
		t.Fatal("renewal failure ignored")
	}
	still, e := c.current()
	if e != nil || !old.Equal(still) {
		t.Fatal("failure discarded valid leaf", e)
	}
	status := c.status()
	if status.State != "failed" || status.NotAfter == nil || status.NextAttempt == nil {
		t.Fatal("renewal failure lost existing certificate metadata")
	}
	status.NextAttempt = nil
	c.saveStatus(status)
	fake.fail = nil
	if e = c.maintain(context.Background(), false, create); e != nil {
		t.Fatal(e)
	}
	current, e := c.current()
	if e != nil || old.Equal(current) || reflect.DeepEqual(old.PublicKey, current.PublicKey) {
		t.Fatal("renewal did not rotate the key", e)
	}
	if c.status().State != "ready" || c.status().Error != "" {
		t.Fatal("success did not clear failure")
	}
}

func TestUntrustedChainIsRejected(t *testing.T) {
	c := config(t)
	c.ensureDirs()
	cert, key := pair(t, c.ACME.Domains, time.Now())
	c.trustRoots = x509.NewCertPool()
	if _, e := c.publish(cert, key); e == nil {
		t.Fatal("untrusted certificate chain was published")
	}
}
