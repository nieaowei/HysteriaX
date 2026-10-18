package main

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/pem"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/caddyserver/certmagic"
	"github.com/libdns/libdns"
	"github.com/miekg/dns"
	"go.uber.org/zap"
)

// Opt-in integration tests use only an isolated Pebble CA and an in-process DNS
// provider. They never contact a production CA or a real DNS account.
func TestPebbleChallenges(t *testing.T) {
	binary := os.Getenv("PEBBLE_BIN")
	if binary == "" {
		t.Skip("set PEBBLE_BIN to the pinned Pebble test binary")
	}
	for _, kind := range []string{"http", "tls", "dns"} {
		t.Run(kind, func(t *testing.T) {
			dir := t.TempDir()
			stub := &testDNS{records: map[string][]string{}}
			packet, e := net.ListenPacket("udp", "127.0.0.1:0")
			if e != nil {
				t.Fatal(e)
			}
			dnsServer := &dns.Server{PacketConn: packet, Handler: stub}
			go dnsServer.ActivateAndServe()
			defer dnsServer.Shutdown()
			tcp, err := net.Listen("tcp", packet.LocalAddr().String())
			if err != nil {
				t.Fatal(err)
			}
			tcpServer := &dns.Server{Listener: tcp, Handler: stub}
			go tcpServer.ActivateAndServe()
			defer tcpServer.Shutdown()
			// httptest's dedicated loopback certificate is trusted only by this test.
			temp := httptest.NewTLSServer(http.NotFoundHandler())
			certificate := temp.TLS.Certificates[0]
			temp.Close()
			certPath := filepath.Join(dir, "ca-server.pem")
			keyPath := filepath.Join(dir, "ca-server.key")
			os.WriteFile(certPath, pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: certificate.Certificate[0]}), 0600)
			key, _ := x509.MarshalPKCS8PrivateKey(certificate.PrivateKey)
			os.WriteFile(keyPath, pem.EncodeToMemory(&pem.Block{Type: "PRIVATE KEY", Bytes: key}), 0600)
			roots := x509.NewCertPool()
			roots.AppendCertsFromPEM(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: certificate.Certificate[0]}))
			free := func() int {
				l, e := net.Listen("tcp", "127.0.0.1:0")
				if e != nil {
					t.Fatal(e)
				}
				defer l.Close()
				return l.Addr().(*net.TCPAddr).Port
			}
			apiPort, httpPort, tlsPort, managementPort := free(), free(), free(), free()
			cfgPath := filepath.Join(dir, "pebble.json")
			writeJSON(cfgPath, map[string]any{"pebble": map[string]any{"listenAddress": fmt.Sprintf("127.0.0.1:%d", apiPort), "certificate": certPath, "privateKey": keyPath, "managementListenAddress": fmt.Sprintf("127.0.0.1:%d", managementPort), "httpPort": httpPort, "tlsPort": tlsPort, "domainBlocklist": []string{"blocked.example"}, "retryAfter": map[string]int{"authz": 1, "order": 1}}}, 0600)
			log, _ := os.Create(filepath.Join(dir, "pebble.log"))
			defer log.Close()
			cmd := exec.Command(binary, "-config", cfgPath, "-dnsserver", packet.LocalAddr().String())
			cmd.Env = append(os.Environ(), "PEBBLE_VA_NOSLEEP=1", "PEBBLE_WFE_NONCEREJECT=0", "PEBBLE_AUTHZREUSE=0")
			cmd.Stdout = log
			cmd.Stderr = log
			if e = cmd.Start(); e != nil {
				t.Fatal(e)
			}
			defer func() {
				cmd.Process.Kill()
				cmd.Wait()
				if t.Failed() {
					b, _ := os.ReadFile(log.Name())
					t.Log(string(b))
				}
			}()
			endpoint := fmt.Sprintf("https://127.0.0.1:%d/dir", apiPort)
			ready := false
			for range 100 {
				conn, e := net.DialTimeout("tcp", fmt.Sprintf("127.0.0.1:%d", apiPort), time.Millisecond*50)
				if e == nil {
					conn.Close()
					ready = true
					break
				}
				time.Sleep(time.Millisecond * 50)
			}
			if !ready {
				t.Fatal("Pebble did not start")
			}
			client := &http.Client{Transport: &http.Transport{TLSClientConfig: &tls.Config{RootCAs: roots}}, Timeout: 5 * time.Second}
			response, err := client.Get(fmt.Sprintf("https://127.0.0.1:%d/roots/0", managementPort))
			if err != nil {
				t.Fatal(err)
			}
			rootPEM, err := io.ReadAll(response.Body)
			response.Body.Close()
			if err != nil || !isolatedRoots.AppendCertsFromPEM(rootPEM) {
				t.Fatal("isolated CA root is unavailable", err)
			}
			c := config(t)
			c.ensureDirs()
			c.ACME.Type = kind
			c.ACME.ListenHost = "127.0.0.1"
			if kind == "http" {
				c.ACME.HTTP = &Challenge{AltPort: httpPort}
			}
			if kind == "tls" {
				c.ACME.TLS = &Challenge{AltPort: tlsPort}
			}
			caConfig := &certmagic.Config{Storage: &certmagic.FileStorage{Path: c.storageDir()}, DisableARI: true, Logger: zap.NewNop()}
			template := certmagic.ACMEIssuer{CA: endpoint, Email: "test@example.com", Agreed: true, TrustedRoots: roots, ListenHost: "127.0.0.1", Logger: zap.NewNop(), AltHTTPPort: httpPort, AltTLSALPNPort: tlsPort, DisableHTTPChallenge: kind != "http", DisableTLSALPNChallenge: kind != "tls"}
			if kind == "dns" {
				template.DNS01Solver = &certmagic.DNS01Solver{DNSManager: certmagic.DNSManager{DNSProvider: stub, Resolvers: []string{packet.LocalAddr().String()}, PropagationTimeout: 10 * time.Second}}
			}
			real := certmagic.NewACMEIssuer(caConfig, template)
			real.TestCA = ""
			ctx, cancel := context.WithTimeout(context.Background(), 45*time.Second)
			defer cancel()
			if e = c.maintain(ctx, false, func(context.Context) (issuer, error) { return testIssuer{real, t}, nil }); e != nil {
				t.Fatal(e)
			}
			leaf, e := c.current()
			if e != nil || len(leaf.DNSNames) != 2 {
				t.Fatal("multi-domain issuance failed", e)
			}
			// The deployment process can go away: a fresh component configuration reuses
			// the durable publication and doesn't require another issuance.
			if e = c.maintain(ctx, true, func(context.Context) (issuer, error) { t.Fatal("restart reissued a valid SAN"); return nil, nil }); e != nil {
				t.Fatal(e)
			}
			bad := config(t)
			bad.ensureDirs()
			bad.ACME.Type = kind
			bad.ACME.ListenHost = "127.0.0.1"
			bad.ACME.Domains = []string{"blocked.example", "b.example"}
			if kind == "http" {
				bad.ACME.HTTP = &Challenge{AltPort: httpPort}
			}
			if kind == "tls" {
				bad.ACME.TLS = &Challenge{AltPort: tlsPort}
			}
			if err := bad.maintain(ctx, false, func(context.Context) (issuer, error) { return real, nil }); err == nil {
				t.Fatal("isolated CA denial was ignored")
			}
			failed := bad.status()
			if failed.State != "failed" || failed.Failures != 1 || failed.NextAttempt == nil {
				t.Fatal("CA denial lost retry progress")
			}
			if err := bad.maintain(ctx, false, func(context.Context) (issuer, error) {
				t.Fatal("restart ignored CA failure retry schedule")
				return nil, nil
			}); err == nil {
				t.Fatal("scheduled failure was ignored")
			}

		})
	}
}

type testDNS struct {
	mu      sync.Mutex
	records map[string][]string
}

func (d *testDNS) AppendRecords(_ context.Context, zone string, records []libdns.Record) ([]libdns.Record, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, r := range records {
		rr := r.RR()
		name := strings.TrimSuffix(strings.ToLower(libdns.AbsoluteName(rr.Name, zone)), ".")
		d.records[name] = append(d.records[name], rr.Data)
	}
	return records, nil
}
func (d *testDNS) DeleteRecords(_ context.Context, zone string, records []libdns.Record) ([]libdns.Record, error) {
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, r := range records {
		rr := r.RR()
		delete(d.records, strings.TrimSuffix(strings.ToLower(libdns.AbsoluteName(rr.Name, zone)), "."))
	}
	return records, nil
}
func (d *testDNS) ServeDNS(w dns.ResponseWriter, r *dns.Msg) {
	answer := new(dns.Msg)
	answer.SetReply(r)
	answer.Authoritative = true
	d.mu.Lock()
	defer d.mu.Unlock()
	for _, q := range r.Question {
		name := strings.ToLower(q.Name)
		switch q.Qtype {
		case dns.TypeA:
			answer.Answer = append(answer.Answer, &dns.A{Hdr: dns.RR_Header{Name: q.Name, Rrtype: dns.TypeA, Class: dns.ClassINET, Ttl: 1}, A: net.IPv4(127, 0, 0, 1)})
		case dns.TypeSOA:
			if name == "example." {
				answer.Answer = append(answer.Answer, &dns.SOA{Hdr: dns.RR_Header{Name: "example.", Rrtype: dns.TypeSOA, Class: dns.ClassINET, Ttl: 1}, Ns: "ns.example.", Mbox: "ops.example.", Serial: 1, Refresh: 1, Retry: 1, Expire: 60, Minttl: 1})
			}
		case dns.TypeTXT:
			for _, value := range d.records[strings.TrimSuffix(name, ".")] {
				answer.Answer = append(answer.Answer, &dns.TXT{Hdr: dns.RR_Header{Name: q.Name, Rrtype: dns.TypeTXT, Class: dns.ClassINET, Ttl: 1}, Txt: []string{value}})
			}
		}
	}
	w.WriteMsg(answer)
}

type testIssuer struct {
	*certmagic.ACMEIssuer
	t *testing.T
}

func (i testIssuer) Issue(ctx context.Context, csr *x509.CertificateRequest) (*certmagic.IssuedCertificate, error) {
	cert, e := i.ACMEIssuer.Issue(ctx, csr)
	if e != nil {
		i.t.Logf("isolated CA error: %v", e)
	}
	return cert, e
}
