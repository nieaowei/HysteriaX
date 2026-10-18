package main

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"math/big"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/caddyserver/certmagic"
	"golang.org/x/sys/unix"
)

type Status struct {
	SchemaVersion    int        `json:"schema_version"`
	ComponentVersion string     `json:"component_version"`
	PolicyID         string     `json:"policy_id"`
	State            string     `json:"state"`
	Domains          []string   `json:"domains"`
	Fingerprint      string     `json:"fingerprint,omitempty"`
	NotBefore        *time.Time `json:"not_before,omitempty"`
	NotAfter         *time.Time `json:"not_after,omitempty"`
	LastSuccess      *time.Time `json:"last_success,omitempty"`
	NextAttempt      *time.Time `json:"next_attempt,omitempty"`
	Error            string     `json:"error,omitempty"`
	Failures         int        `json:"failures"`
	CollectedAt      time.Time  `json:"collected_at"`
}

func (c Config) status() Status {
	s := Status{SchemaVersion: 1, ComponentVersion: version, PolicyID: c.PolicyID, Domains: c.ACME.Domains, State: "pending"}
	b, e := os.ReadFile(filepath.Join(c.StateDir, "status.json"))
	if e == nil {
		var old Status
		if json.Unmarshal(b, &old) == nil && old.PolicyID == c.PolicyID {
			s = old
		}
	}
	s.ComponentVersion = version
	if s.NotAfter != nil && time.Now().After(*s.NotAfter) {
		s.State = "expired"
	}
	s.CollectedAt = time.Now().UTC()
	if leaf, e := c.current(); e == nil {
		s.setCertificate(leaf)
		if time.Now().After(leaf.NotAfter) {
			s.State = "expired"
		}
	}
	return s
}
func (s *Status) setCertificate(leaf *x509.Certificate) {
	h := sha256.Sum256(leaf.Raw)
	s.Fingerprint = hex.EncodeToString(h[:])
	s.NotBefore = &leaf.NotBefore
	s.NotAfter = &leaf.NotAfter
}
func (c Config) saveStatus(s Status) error {
	s.CollectedAt = time.Now().UTC()
	return writeJSON(filepath.Join(c.StateDir, "status.json"), s, 0600)
}
func writeJSON(path string, v any, mode os.FileMode) error {
	b, e := json.Marshal(v)
	if e != nil {
		return e
	}
	return atomicFile(path, append(b, '\n'), mode)
}
func atomicFile(path string, b []byte, mode os.FileMode) error {
	f, e := os.CreateTemp(filepath.Dir(path), ".write-*")
	if e != nil {
		return e
	}
	defer os.Remove(f.Name())
	defer f.Close()
	if e = f.Chmod(mode); e != nil {
		return e
	}
	if _, e = f.Write(b); e != nil {
		return e
	}
	if e = f.Sync(); e != nil {
		return e
	}
	if e = f.Close(); e != nil {
		return e
	}
	if e = os.Rename(f.Name(), path); e != nil {
		return e
	}
	return syncDir(filepath.Dir(path))
}
func syncDir(path string) error {
	d, e := os.Open(path)
	if e != nil {
		return e
	}
	defer d.Close()
	return d.Sync()
}
func (c Config) lock() (*os.File, error) {
	path := filepath.Join(filepath.Dir(c.StateDir), "node.lock")
	if os.Getenv("HYSTERIAX_ACME_LOCK_FD") == "9" {
		f := os.NewFile(9, "deployment-lock")
		s, e := f.Stat()
		if e != nil {
			return nil, e
		}
		p, e := os.Stat(path)
		if e != nil || !os.SameFile(s, p) {
			return nil, errors.New("invalid inherited lock")
		}
		return f, nil
	}
	f, e := os.OpenFile(path, os.O_CREATE|os.O_RDWR, 0600)
	if e != nil {
		return nil, e
	}
	if e = unix.Flock(int(f.Fd()), unix.LOCK_EX|unix.LOCK_NB); e != nil {
		f.Close()
		return nil, errors.New("node certificate writer is busy")
	}
	return f, nil
}
func (c Config) current() (*x509.Certificate, error) {
	// Resolve once so both reads belong to the same immutable generation.
	dir, e := filepath.EvalSymlinks(filepath.Join(c.PublishDir, "current"))
	if e != nil {
		return nil, e
	}
	versions, err := filepath.EvalSymlinks(filepath.Join(c.PublishDir, "versions"))
	if err != nil || filepath.Dir(dir) != versions {
		return nil, errors.New("invalid certificate publication pointer")
	}
	b, e := os.ReadFile(filepath.Join(dir, "cert.pem"))
	if e != nil {
		return nil, e
	}
	k, e := os.ReadFile(filepath.Join(dir, "key.pem"))
	if e != nil {
		return nil, e
	}
	return c.validatePair(b, k, time.Now())
}
func validatePair(cert, key []byte, names []string, now time.Time) (*x509.Certificate, error) {
	pair, e := tls.X509KeyPair(cert, key)
	if e != nil {
		return nil, errors.New("certificate and private key do not match")
	}
	if len(pair.Certificate) < 2 {
		return nil, errors.New("certificate chain is incomplete")
	}
	leaf, e := x509.ParseCertificate(pair.Certificate[0])
	if e != nil {
		return nil, e
	}
	if now.Before(leaf.NotBefore) || !now.Before(leaf.NotAfter) {
		return nil, errors.New("certificate is outside its validity period")
	}
	for _, n := range names {
		found := false
		for _, san := range leaf.DNSNames {
			if strings.EqualFold(n, san) {
				found = true
			}
		}
		if !found {
			return nil, errors.New("certificate does not cover the complete domain set")
		}
	}
	for i := 1; i < len(pair.Certificate); i++ {
		issuer, e := x509.ParseCertificate(pair.Certificate[i])
		if e != nil {
			return nil, e
		}
		child, e := x509.ParseCertificate(pair.Certificate[i-1])
		if e != nil {
			return nil, e
		}
		if e = child.CheckSignatureFrom(issuer); e != nil {
			return nil, errors.New("invalid certificate chain signature")
		}
		if now.Before(issuer.NotBefore) || !now.Before(issuer.NotAfter) {
			return nil, errors.New("issuer outside its validity period")
		}
	}
	return leaf, nil
}
func (c Config) validatePair(cert, key []byte, now time.Time) (*x509.Certificate, error) {
	leaf, e := validatePair(cert, key, c.ACME.Domains, now)
	if e != nil {
		return nil, e
	}
	roots := c.trustRoots
	if roots == nil {
		roots, e = x509.SystemCertPool()
		if e != nil {
			return nil, errors.New("system certificate trust store is unavailable")
		}
	}
	intermediates := x509.NewCertPool()
	rest := cert
	for {
		block, next := pem.Decode(rest)
		if block == nil {
			break
		}
		rest = next
		if block.Type != "CERTIFICATE" {
			continue
		}
		issuer, err := x509.ParseCertificate(block.Bytes)
		if err != nil {
			return nil, errors.New("invalid certificate chain")
		}
		if !issuer.Equal(leaf) {
			intermediates.AddCert(issuer)
		}
	}
	if _, e = leaf.Verify(x509.VerifyOptions{Roots: roots, Intermediates: intermediates, CurrentTime: now, KeyUsages: []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth}}); e != nil {
		return nil, errors.New("certificate chain is not trusted for server authentication")
	}
	return leaf, nil
}

func renewalTime(c *x509.Certificate) time.Time {
	return c.NotAfter.Add(-c.NotAfter.Sub(c.NotBefore) / 3)
}
func backoff(failures int) time.Duration {
	if failures < 1 {
		failures = 1
	}
	if failures > 10 {
		failures = 10
	}
	base := time.Minute * time.Duration(1<<uint(failures-1))
	if base >= 6*time.Hour {
		base = 5 * time.Hour
	}
	n, e := rand.Int(rand.Reader, big.NewInt(int64(base/5)+1))
	if e == nil {
		base += time.Duration(n.Int64())
	}
	if base > 6*time.Hour {
		base = 6 * time.Hour
	}
	return base
}
func (c Config) publish(cert, key []byte) (*x509.Certificate, error) {
	leaf, e := c.validatePair(cert, key, time.Now())
	if e != nil {
		return nil, e
	}
	versions := filepath.Join(c.PublishDir, "versions")
	if e = os.MkdirAll(versions, 0750); e != nil {
		return nil, e
	}
	h := sha256.Sum256(leaf.Raw)
	name := hex.EncodeToString(h[:])
	dir := filepath.Join(versions, name)
	if e = os.Mkdir(dir, 0750); e != nil && !os.IsExist(e) {
		return nil, e
	}
	// A generation is immutable once complete. Interrupted staging can be safely rebuilt.
	for _, f := range []struct {
		name string
		b    []byte
	}{{"cert.pem", cert}, {"key.pem", key}} {
		path := filepath.Join(dir, f.name)
		old, err := os.ReadFile(path)
		if err == nil {
			if string(old) != string(f.b) {
				return nil, errors.New("immutable certificate generation conflict")
			}
			continue
		}
		if !os.IsNotExist(err) {
			return nil, err
		}
		if e = atomicFile(path, f.b, 0640); e != nil {
			return nil, e
		}
	}
	if e = syncDir(versions); e != nil {
		return nil, e
	}
	// Durable intent allows startup to finish an interrupted pointer replacement.
	if e = writeJSON(filepath.Join(c.StateDir, "publish.json"), name, 0600); e != nil {
		return nil, e
	}
	if e = c.point(name); e != nil {
		return nil, e
	}
	if e = os.Remove(filepath.Join(c.StateDir, "publish.json")); e != nil {
		return nil, e
	}
	return leaf, nil
}
func (c Config) point(name string) error {
	if len(name) != 64 || strings.Trim(name, "0123456789abcdef") != "" {
		return errors.New("invalid generation")
	}
	dir := filepath.Join(c.PublishDir, "versions", name)
	b, e := os.ReadFile(filepath.Join(dir, "cert.pem"))
	if e != nil {
		return e
	}
	k, e := os.ReadFile(filepath.Join(dir, "key.pem"))
	if e != nil {
		return e
	}
	if _, e = c.validatePair(b, k, time.Now()); e != nil {
		return e
	}
	tmp := filepath.Join(c.PublishDir, ".current-new")
	_ = os.Remove(tmp)
	if e = os.Symlink(filepath.Join("versions", name), tmp); e != nil {
		return e
	}
	if e = os.Rename(tmp, filepath.Join(c.PublishDir, "current")); e != nil {
		return e
	}
	return syncDir(c.PublishDir)
}
func (c Config) recover() error {
	b, e := os.ReadFile(filepath.Join(c.StateDir, "publish.json"))
	if os.IsNotExist(e) {
		return nil
	}
	if e != nil {
		return e
	}
	var name string
	if e = json.Unmarshal(b, &name); e != nil {
		return e
	}
	if e = c.point(name); e != nil {
		return e
	}
	return os.Remove(filepath.Join(c.StateDir, "publish.json"))
}
func newCSR(names []string) (*x509.CertificateRequest, []byte, error) {
	key, e := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if e != nil {
		return nil, nil, e
	}
	der, e := x509.CreateCertificateRequest(rand.Reader, &x509.CertificateRequest{DNSNames: names}, key)
	if e != nil {
		return nil, nil, e
	}
	csr, e := x509.ParseCertificateRequest(der)
	if e != nil {
		return nil, nil, e
	}
	b, e := x509.MarshalPKCS8PrivateKey(key)
	return csr, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: b}), e
}

// The injected issuer keeps isolated CA tests independent of production services.
type issuer interface {
	PreCheck(context.Context, []string, bool) error
	Issue(context.Context, *x509.CertificateRequest) (*certmagic.IssuedCertificate, error)
}

func (c Config) maintain(ctx context.Context, ensure bool, newIssuer func(context.Context) (issuer, error)) error {
	lock, e := c.lock()
	if e != nil {
		return e
	}
	defer lock.Close()
	if e = c.recover(); e != nil {
		return errors.New("certificate publication recovery failed")
	}
	s := c.status()
	leaf, currentErr := c.current()
	if currentErr != nil {
		// Reuse an existing compatible SAN certificate from Hy2's CertMagic layout.
		files, _ := filepath.Glob(filepath.Join(c.storageDir(), "certificates", "*", "*", "*.crt"))
		for _, path := range files {
			cert, ce := os.ReadFile(path)
			key, ke := os.ReadFile(strings.TrimSuffix(path, ".crt") + ".key")
			if ce != nil || ke != nil {
				continue
			}
			candidate, err := c.validatePair(cert, key, time.Now())
			if err != nil {
				continue
			}
			if _, err = c.publish(cert, key); err != nil {
				return errors.New("legacy certificate publication failed")
			}
			leaf, currentErr = candidate, nil
			break
		}
	}
	if currentErr == nil && (ensure || time.Now().Before(renewalTime(leaf))) {
		if s.Failures == 0 {
			s.State = "ready"
			s.Error = ""
		}
		return c.saveStatus(s)
	}
	if s.NextAttempt != nil && time.Now().Before(*s.NextAttempt) {
		return errors.New("certificate retry is scheduled")
	}
	s.State = "issuing"
	if leaf != nil {
		s.State = "renewing"
	}
	if e = c.saveStatus(s); e != nil {
		return e
	}
	var iss issuer
	e = c.checkChallengePort()
	if e == nil {
		iss, e = newIssuer(ctx)
	}
	if e != nil {
		iss = nil
	}
	var cert *certmagic.IssuedCertificate
	var key []byte
	if e == nil {
		var csr *x509.CertificateRequest
		csr, key, e = newCSR(c.ACME.Domains)
		if e == nil {
			e = iss.PreCheck(ctx, c.ACME.Domains, false)
		}
		if e == nil {
			cert, e = iss.Issue(ctx, csr)
		}
	}
	if e == nil {
		leaf, e = c.publish(cert.Certificate, key)
	}
	if e != nil {
		s.State = "failed"
		s.Failures++
		next := time.Now().UTC().Add(backoff(s.Failures))
		if retry, ok := iss.(interface{ retryDeadline() time.Time }); ok && retry.retryDeadline().After(next) {
			next = retry.retryDeadline()
		}
		var delayed interface{ retryDeadline() time.Time }
		if errors.As(e, &delayed) && delayed.retryDeadline().After(next) {
			next = delayed.retryDeadline()
		}
		s.NextAttempt = &next
		// Provider and CA errors can include credential-bearing URLs or response bodies.
		s.Error = "Certificate issuance failed; check domain validation, challenge ports, DNS credentials and CA availability."
		var portError challengePortError
		if errors.As(e, &portError) {
			s.Error = portError.Error()
		}
		if errors.Is(ctx.Err(), context.DeadlineExceeded) {
			s.Error = "Certificate issuance timed out."
		}
		if err := c.saveStatus(s); err != nil {
			return err
		}
		return errors.New(s.Error)
	}
	s.setCertificate(leaf)
	s.State = "ready"
	s.Failures = 0
	s.Error = ""
	s.NextAttempt = nil
	now := time.Now().UTC()
	s.LastSuccess = &now
	return c.saveStatus(s)
}
func (c Config) ensureDirs() error {
	for _, d := range []struct {
		p string
		m os.FileMode
	}{{c.StateDir, 0700}, {c.PublishDir, 0750}} {
		if e := os.MkdirAll(d.p, d.m); e != nil {
			return fmt.Errorf("create component directory: %w", e)
		}
	}
	return nil
}
