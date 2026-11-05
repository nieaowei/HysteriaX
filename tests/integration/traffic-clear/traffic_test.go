package trafficclear

import (
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/apernet/hysteria/extras/v2/trafficlogger"
)

type counter struct {
	Tx uint64 `json:"tx"`
	Rx uint64 `json:"rx"`
}

func read(t *testing.T, server *httptest.Server, path string, target any) {
	t.Helper()
	request, err := http.NewRequest(http.MethodGet, server.URL+path, nil)
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "test-secret")
	response, err := server.Client().Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		t.Fatalf("%s: status %d", path, response.StatusCode)
	}
	if err := json.NewDecoder(response.Body).Decode(target); err != nil {
		t.Fatal(err)
	}
}

func TestPinnedHandlerClearPreservesOnlineAndContinuedTraffic(t *testing.T) {
	logger := trafficlogger.NewTrafficStatsServer("test-secret")
	server := httptest.NewServer(logger)
	defer server.Close()
	logger.LogOnlineState("user-0001", true)
	for round := 0; round < 3; round++ {
		for i := 1; i <= 1000; i++ {
			logger.LogTraffic(fmt.Sprintf("user-%04d", i), 10, 5)
		}
		for i := 0; i < 2000; i++ {
			logger.LogTraffic(fmt.Sprintf("probe-%d-%04d", round, i), 1, 2)
		}
		var counters map[string]counter
		read(t, server, "/traffic?clear=1", &counters)
		if len(counters) != 3000 || counters["user-0001"] != (counter{10, 5}) {
			t.Fatalf("round %d: unexpected counters: count=%d user=%+v", round, len(counters), counters["user-0001"])
		}
		counters = nil
		read(t, server, "/traffic", &counters)
		if len(counters) != 0 {
			t.Fatalf("round %d: cleared identities remain: %d", round, len(counters))
		}
		var online map[string]int
		read(t, server, "/online", &online)
		if online["user-0001"] != 1 {
			t.Fatalf("online connection lost: %v", online)
		}
	}
	logger.LogTraffic("user-0001", 7, 3)
	var counters map[string]counter
	read(t, server, "/traffic?clear=1", &counters)
	if counters["user-0001"] != (counter{7, 3}) {
		t.Fatalf("continued connection was not counted: %v", counters)
	}
}
