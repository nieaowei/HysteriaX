package main

import (
	"context"
	"crypto/x509"
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/caddyserver/certmagic"
	"github.com/libdns/cloudflare"
	"github.com/libdns/duckdns"
	"github.com/libdns/gandi"
	"github.com/libdns/godaddy"
	"github.com/libdns/namecheap"
	"github.com/libdns/njalla"
	"github.com/libdns/porkbun"
	"github.com/libdns/vultr/v2"
	"github.com/mholt/acmez/v3/acme"
	"go.uber.org/zap"
)

const version = "0.1.0"

type Config struct {
	// Tests can trust their isolated CA in memory; JSON cannot set this field.
	trustRoots *x509.CertPool
	PolicyID   string  `json:"policy_id"`
	StateDir   string  `json:"state_dir"`
	PublishDir string  `json:"publish_dir"`
	ACME       Options `json:"acme"`
}
type Options struct {
	Domains        []string   `json:"domains"`
	Email          string     `json:"email"`
	CA             string     `json:"ca"`
	ListenHost     string     `json:"listenHost"`
	Dir            string     `json:"dir"`
	Type           string     `json:"type"`
	HTTP           *Challenge `json:"http,omitempty"`
	TLS            *Challenge `json:"tls,omitempty"`
	DNS            *DNS       `json:"dns,omitempty"`
	DisableHTTP    bool       `json:"disableHTTP"`
	DisableTLSALPN bool       `json:"disableTLSALPN"`
	AltHTTPPort    int        `json:"altHTTPPort"`
	AltTLSALPNPort int        `json:"altTLSALPNPort"`
}
type Challenge struct {
	AltPort int `json:"altPort"`
}
type DNS struct {
	Name   string            `json:"name"`
	Config map[string]string `json:"config"`
}

func loadConfig(path string) (Config, error) {
	var c Config
	f, e := os.Open(path)
	if e != nil {
		return c, e
	}
	defer f.Close()
	d := json.NewDecoder(io.LimitReader(f, 1<<20))
	d.DisallowUnknownFields()
	if e = d.Decode(&c); e != nil {
		return c, errors.New("invalid component configuration")
	}
	if e = d.Decode(new(any)); e != io.EOF {
		return c, errors.New("unexpected trailing configuration")
	}
	return c, c.check()
}
func (c Config) check() error {
	if len(c.PolicyID) != 64 || strings.Trim(c.PolicyID, "0123456789abcdef") != "" {
		return errors.New("invalid policy id")
	}
	for _, p := range []string{c.StateDir, c.PublishDir, c.ACME.Dir} {
		if p != "" && (!filepath.IsAbs(p) || filepath.Clean(p) != p || strings.ContainsAny(p, "\n\r\x00")) {
			return errors.New("component directories must be clean absolute paths")
		}
	}
	if c.StateDir == "" || c.PublishDir == "" {
		return errors.New("component directories are required")
	}
	a := c.ACME
	a.Type = strings.ToLower(a.Type)
	if len(a.Domains) == 0 {
		return errors.New("domains are required")
	}
	seen := map[string]bool{}
	for _, n := range a.Domains {
		if n == "" || strings.ContainsAny(n, " /:\t\r\n") || seen[strings.ToLower(n)] {
			return errors.New("invalid or duplicate domain")
		}
		seen[strings.ToLower(n)] = true
	}
	switch strings.ToLower(a.CA) {
	case "", "le", "letsencrypt", "zero", "zerossl":
	default:
		return errors.New("unsupported CA")
	}
	for _, p := range []int{a.AltHTTPPort, a.AltTLSALPNPort, port(a.HTTP), port(a.TLS)} {
		if p < 0 || p > 65535 {
			return errors.New("invalid challenge port")
		}
	}
	switch strings.ToLower(a.Type) {
	case "":
		if a.HTTP != nil || a.TLS != nil || a.DNS != nil {
			return errors.New("nested challenge requires type")
		}
		if a.DisableHTTP && a.DisableTLSALPN {
			return errors.New("all challenges are disabled")
		}
	case "http", "tls", "dns":
		if a.DisableHTTP || a.DisableTLSALPN || a.AltHTTPPort != 0 || a.AltTLSALPNPort != 0 {
			return errors.New("legacy challenge fields require empty type")
		}
		if a.Type != "http" && a.HTTP != nil || a.Type != "tls" && a.TLS != nil || a.Type != "dns" && a.DNS != nil {
			return errors.New("conflicting challenge settings")
		}
		if a.Type == "dns" {
			if _, e := provider(a.DNS); e != nil {
				return e
			}
		}
	default:
		return errors.New("unsupported challenge type")
	}
	return nil
}
func port(c *Challenge) int {
	if c == nil {
		return 0
	}
	return c.AltPort
}
func provider(d *DNS) (certmagic.DNSProvider, error) {
	if d == nil {
		return nil, errors.New("DNS configuration is required")
	}
	c := d.Config
	var p certmagic.DNSProvider
	var keys []string
	switch strings.ToLower(d.Name) {
	case "cloudflare":
		keys = []string{"cloudflare_api_token"}
		p = &cloudflare.Provider{APIToken: c[keys[0]]}
	case "duckdns":
		keys = []string{"duckdns_api_token"}
		p = &duckdns.Provider{APIToken: c[keys[0]], OverrideDomain: c["duckdns_override_domain"]}
	case "gandi":
		keys = []string{"gandi_api_token"}
		p = &gandi.Provider{BearerToken: c[keys[0]]}
	case "godaddy":
		keys = []string{"godaddy_api_token"}
		p = &godaddy.Provider{APIToken: c[keys[0]]}
	case "namecheap":
		keys = []string{"namecheap_api_key", "namecheap_api_user"}
		p = &namecheap.Provider{APIKey: c[keys[0]], User: c[keys[1]], APIEndpoint: c["namecheap_api_endpoint"], ClientIP: c["namecheap_client_ip"]}
	case "njalla":
		keys = []string{"njalla_api_token"}
		p = &njalla.Provider{APIToken: c[keys[0]]}
	case "porkbun":
		keys = []string{"porkbun_api_key", "porkbun_api_secret_key"}
		p = &porkbun.Provider{APIKey: c[keys[0]], APISecretKey: c[keys[1]]}
	case "vultr":
		keys = []string{"vultr_api_token"}
		p = &vultr.Provider{APIToken: c[keys[0]]}
	default:
		return nil, errors.New("unsupported DNS provider")
	}
	for _, k := range keys {
		if strings.TrimSpace(c[k]) == "" {
			return nil, errors.New("missing DNS credential: " + k)
		}
	}
	return p, nil
}
func (c Config) storageDir() string {
	if c.ACME.Dir != "" {
		return c.ACME.Dir
	}
	if p := os.Getenv("HYSTERIA_ACME_DIR"); p != "" {
		return p
	}
	return "/var/lib/hysteriax/acme"
}
func (c Config) issuer(ctx context.Context) (*trackedIssuer, error) {
	a := c.ACME
	a.Type = strings.ToLower(a.Type)
	cfg := &certmagic.Config{Storage: &certmagic.FileStorage{Path: c.storageDir()}, Logger: zap.NewNop(), DisableARI: true}
	template := certmagic.ACMEIssuer{Email: a.Email, Agreed: true, ListenHost: a.ListenHost, Logger: zap.NewNop(), CA: certmagic.LetsEncryptProductionCA, DisableHTTPChallenge: a.DisableHTTP, DisableTLSALPNChallenge: a.DisableTLSALPN, AltHTTPPort: a.AltHTTPPort, AltTLSALPNPort: a.AltTLSALPNPort}
	if strings.EqualFold(a.CA, "zerossl") || strings.EqualFold(a.CA, "zero") {
		template.CA = certmagic.ZeroSSLProductionCA
		// Cache EAB privately, never send credentials through argv or logs.
		path := filepath.Join(c.StateDir, "eab.json")
		b, e := os.ReadFile(path)
		var eab acme.EAB
		if e == nil {
			e = json.Unmarshal(b, &eab)
		}
		if e != nil || eab.KeyID == "" || eab.MACKey == "" {
			var err error
			eab, err = zeroSSLEAB(ctx, a.Email, http.DefaultClient)
			if err != nil {
				return nil, err
			}
			if err = writeJSON(path, eab, 0600); err != nil {
				return nil, err
			}
		}
		template.ExternalAccount = &eab
	}
	switch strings.ToLower(a.Type) {
	case "http":
		template.DisableHTTPChallenge = false
		template.DisableTLSALPNChallenge = true
		template.AltHTTPPort = port(a.HTTP)
	case "tls":
		template.DisableHTTPChallenge = true
		template.DisableTLSALPNChallenge = false
		template.AltTLSALPNPort = port(a.TLS)
	case "dns":
		p, e := provider(a.DNS)
		if e != nil {
			return nil, e
		}
		template.DisableHTTPChallenge = true
		template.DisableTLSALPNChallenge = true
		template.DNS01Solver = &certmagic.DNS01Solver{DNSManager: certmagic.DNSManager{DNSProvider: p}}
	}
	observer := &retryObserver{}
	template.Logger = zap.New(observer)
	iss := certmagic.NewACMEIssuer(cfg, template)
	// No staging fallback: retry scheduling belongs to this component.
	iss.TestCA = ""
	return &trackedIssuer{ACMEIssuer: iss, observer: observer}, nil
}
func zeroSSLEAB(ctx context.Context, email string, client *http.Client) (acme.EAB, error) {
	req, e := http.NewRequestWithContext(ctx, http.MethodPost, "https://api.zerossl.com/acme/eab-credentials-email", strings.NewReader(url.Values{"email": {email}}.Encode()))
	if e != nil {
		return acme.EAB{}, errors.New("EAB request failed")
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, e := client.Do(req)
	if e != nil {
		return acme.EAB{}, errors.New("EAB request failed")
	}
	defer resp.Body.Close()
	var r struct {
		Success bool   `json:"success"`
		KeyID   string `json:"eab_kid"`
		MACKey  string `json:"eab_hmac_key"`
	}
	if json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&r) != nil || resp.StatusCode != 200 || !r.Success || r.KeyID == "" || r.MACKey == "" {
		return acme.EAB{}, retryError{after: retryAfter(resp.Header.Get("Retry-After"), time.Now())}
	}
	return acme.EAB{KeyID: r.KeyID, MACKey: r.MACKey}, nil
}

// Check only the selected listener. Legacy mode with both challenges lets
// CertMagic choose/fall back; a busy website port is never taken over.
func (c Config) checkChallengePort() error {
	a := c.ACME
	kind := strings.ToLower(a.Type)
	p := 0
	if kind == "http" {
		p = port(a.HTTP)
		if p == 0 {
			p = 80
		}
	}
	if kind == "tls" {
		p = port(a.TLS)
		if p == 0 {
			p = 443
		}
	}
	if kind == "" && a.DisableTLSALPN {
		kind = "http"
		p = a.AltHTTPPort
		if p == 0 {
			p = 80
		}
	}
	if kind == "" && a.DisableHTTP {
		kind = "tls"
		p = a.AltTLSALPNPort
		if p == 0 {
			p = 443
		}
	}
	if p == 0 {
		return nil
	}
	l, e := net.Listen("tcp", net.JoinHostPort(a.ListenHost, strconv.Itoa(p)))
	if e != nil {
		return challengePortError{kind}
	}
	return l.Close()
}

type challengePortError struct{ kind string }

func (e challengePortError) Error() string {
	return "The " + e.kind + " challenge listener cannot bind. Check port conflicts and listener permissions; public 80/443 must forward to the configured challenge port."
}
