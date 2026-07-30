package main

import (
	"context"
	"encoding/json"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
	"time"

	"gopkg.in/yaml.v3"
)

type roundTripFunc func(*http.Request) (*http.Response, error)

func (function roundTripFunc) RoundTrip(request *http.Request) (*http.Response, error) {
	return function(request)
}

func TestSecurityHeaders(t *testing.T) {
	handler := securityHeaders(http.HandlerFunc(func(writer http.ResponseWriter, _ *http.Request) {
		writer.WriteHeader(http.StatusNoContent)
	}))
	recorder := httptest.NewRecorder()
	handler.ServeHTTP(recorder, httptest.NewRequest(http.MethodGet, "/", nil))

	if got := recorder.Header().Get("X-Frame-Options"); got != "DENY" {
		t.Fatalf("X-Frame-Options = %q", got)
	}
	if got := recorder.Header().Get("Content-Security-Policy"); got == "" {
		t.Fatal("Content-Security-Policy is empty")
	}
}

func TestLoopbackListenValidation(t *testing.T) {
	for _, address := range []string{"127.0.0.1:9080", "[::1]:9080", "localhost:9080"} {
		if !isLoopbackListen(address) {
			t.Fatalf("expected %q to be loopback", address)
		}
	}
	for _, address := range []string{"0.0.0.0:9080", "192.168.1.2:9080", "invalid"} {
		if isLoopbackListen(address) {
			t.Fatalf("expected %q to be rejected", address)
		}
	}
}

func TestCoreExecutableName(t *testing.T) {
	if got := coreExecutableName("windows"); got != "zju-portal-core.exe" {
		t.Fatalf("Windows Core name = %q", got)
	}
	if got := coreExecutableName("linux"); got != "zju-portal-core" {
		t.Fatalf("Unix Core name = %q", got)
	}
}

func TestResolveBundledCoreBinary(t *testing.T) {
	directory := t.TempDir()
	webuiPath := filepath.Join(directory, "sumire")
	corePath := filepath.Join(directory, coreExecutableName(runtime.GOOS))
	if err := os.WriteFile(corePath, []byte("binary"), 0700); err != nil {
		t.Fatal(err)
	}
	resolved, managed, err := resolveCoreBinary("", false, webuiPath, runtime.GOOS)
	if err != nil {
		t.Fatal(err)
	}
	if !managed || resolved != corePath {
		t.Fatalf("resolved Core = %q, managed = %t", resolved, managed)
	}
}

func TestResolveExternalCoreMode(t *testing.T) {
	resolved, managed, err := resolveCoreBinary("", true, "/app/sumire", "linux")
	if err != nil {
		t.Fatal(err)
	}
	if managed || resolved != "" {
		t.Fatalf("resolved Core = %q, managed = %t", resolved, managed)
	}
	if _, _, err := resolveCoreBinary("zju-portal-core", true, "/app/sumire", "linux"); err == nil {
		t.Fatal("expected conflicting Core mode flags to fail")
	}
}

func TestHandlerServesUIAndProxiesAPI(t *testing.T) {
	originalTransport := http.DefaultTransport
	http.DefaultTransport = roundTripFunc(func(request *http.Request) (*http.Response, error) {
		if request.Header.Get("Origin") != "" {
			t.Fatalf("proxy forwarded Origin header %q", request.Header.Get("Origin"))
		}
		if request.Header.Get("Authorization") != "Bearer test-token" {
			t.Fatalf("Authorization header = %q", request.Header.Get("Authorization"))
		}
		return &http.Response{
			StatusCode: http.StatusOK,
			Header:     http.Header{"Content-Type": []string{"application/json"}},
			Body:       io.NopCloser(strings.NewReader(`{"result":{"protocol_version":1}}`)),
			Request:    request,
		}, nil
	})
	defer func() { http.DefaultTransport = originalTransport }()

	target, err := url.Parse("http://core.invalid")
	if err != nil {
		t.Fatal(err)
	}
	assets, err := fs.Sub(webFiles, "web")
	if err != nil {
		t.Fatal(err)
	}
	handler := newHandler(target, assets, nil)

	uiRecorder := httptest.NewRecorder()
	handler.ServeHTTP(uiRecorder, httptest.NewRequest(http.MethodGet, "/", nil))
	if uiRecorder.Code != http.StatusOK || !strings.Contains(uiRecorder.Body.String(), "Sumire") {
		t.Fatalf("UI response: status=%d body=%q", uiRecorder.Code, uiRecorder.Body.String())
	}

	apiRequest := httptest.NewRequest(http.MethodGet, "/api/v1/hello", nil)
	apiRequest.Header.Set("Origin", "http://webui.invalid")
	apiRequest.Header.Set("Authorization", "Bearer test-token")
	apiRecorder := httptest.NewRecorder()
	handler.ServeHTTP(apiRecorder, apiRequest)
	if apiRecorder.Code != http.StatusOK || !strings.Contains(apiRecorder.Body.String(), `"protocol_version":1`) {
		t.Fatalf("API response: status=%d body=%q", apiRecorder.Code, apiRecorder.Body.String())
	}
}

func TestNormalizeJSONConfigForManagedCore(t *testing.T) {
	jsonData, yamlData, err := normalizeJSONConfig([]byte(`{
  "version": 1,
  "atrust": {"server": "vpn.example.edu", "port": 443},
  "control": {"rest": {"enabled": false, "listen": "127.0.0.1:1", "secret": "old"}},
  "state": {"resume-file": "old-state"}
}`), "/data/resume.json", "/data/control.token", "127.0.0.1:9090")
	if err != nil {
		t.Fatal(err)
	}
	var jsonConfig map[string]any
	if err := json.Unmarshal(jsonData, &jsonConfig); err != nil {
		t.Fatal(err)
	}
	assertManagedConfig(t, jsonConfig, "/data/resume.json", "/data/control.token")
	var yamlConfig map[string]any
	if err := yaml.Unmarshal(yamlData, &yamlConfig); err != nil {
		t.Fatal(err)
	}
	assertManagedConfig(t, yamlConfig, "/data/resume.json", "/data/control.token")
	if port := yamlConfig["atrust"].(map[string]any)["port"]; port != 443 {
		t.Fatalf("atrust.port = %#v", port)
	}
}

func TestApplyConfigPersistsAfterCoreSuccess(t *testing.T) {
	originalTransport := http.DefaultTransport
	http.DefaultTransport = roundTripFunc(func(request *http.Request) (*http.Response, error) {
		if request.Method != http.MethodPut || request.URL.Path != "/api/v1/config" {
			t.Fatalf("upstream request = %s %s", request.Method, request.URL.Path)
		}
		if request.Header.Get("Authorization") != "Bearer managed-token" {
			t.Fatalf("Authorization = %q", request.Header.Get("Authorization"))
		}
		return &http.Response{
			StatusCode: http.StatusOK,
			Header:     http.Header{"Content-Type": []string{"application/json"}},
			Body:       io.NopCloser(strings.NewReader(`{"result":{"config":{}}}`)),
			Request:    request,
		}, nil
	})
	defer func() { http.DefaultTransport = originalTransport }()

	directory := t.TempDir()
	target, _ := url.Parse("http://core.invalid")
	supervisor := newCoreSupervisor("unused", filepath.Join(directory, "config.yaml"), filepath.Join(directory, "resume.json"), filepath.Join(directory, "control.token"), "127.0.0.1:9090", target)
	status, _, _, err := supervisor.ApplyConfig(context.Background(), "Bearer managed-token", []byte(`{"version":1,"session":{"auto-start":false}}`))
	if err != nil {
		t.Fatal(err)
	}
	if status != http.StatusOK {
		t.Fatalf("status = %d", status)
	}
	data, err := os.ReadFile(supervisor.configFile)
	if err != nil {
		t.Fatal(err)
	}
	var config map[string]any
	if err := yaml.Unmarshal(data, &config); err != nil {
		t.Fatal(err)
	}
	assertManagedConfig(t, config, supervisor.resumeFile, supervisor.tokenFile)
}

func TestPrepareUsesEmbeddedInitialConfiguration(t *testing.T) {
	directory := t.TempDir()
	configFile := filepath.Join(directory, "data", "config.yaml")
	resumeFile := filepath.Join(directory, "data", "resume.json")
	tokenFile := filepath.Join(directory, "data", "control.token")
	target, _ := url.Parse("http://127.0.0.1:9090")
	supervisor := newCoreSupervisor("unused", configFile, resumeFile, tokenFile, "127.0.0.1:9090", target)
	if err := supervisor.Prepare(); err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(configFile)
	if err != nil {
		t.Fatal(err)
	}
	var config map[string]any
	if err := yaml.Unmarshal(data, &config); err != nil {
		t.Fatal(err)
	}
	if config["session"].(map[string]any)["id"] != "default" || config["atrust"].(map[string]any)["server"] != "vpn.zju.edu.cn" {
		t.Fatalf("embedded fields were not preserved: %#v", config)
	}
	text := string(data)
	if !strings.Contains(text, "# 日志级别 info/debug") || !strings.Contains(text, "# 是否创建系统 TUN 设备") {
		t.Fatal("embedded configuration comments were not preserved")
	}
	if strings.Index(text, "log:") > strings.Index(text, "control:") || strings.Index(text, "control:") > strings.Index(text, "session:") {
		t.Fatal("embedded configuration section order was not preserved")
	}
	assertManagedConfig(t, config, resumeFile, tokenFile)
}

func TestApplyYAMLConfigPreservesComments(t *testing.T) {
	originalTransport := http.DefaultTransport
	http.DefaultTransport = roundTripFunc(func(request *http.Request) (*http.Response, error) {
		return &http.Response{
			StatusCode: http.StatusOK,
			Header:     http.Header{"Content-Type": []string{"application/json"}},
			Body:       io.NopCloser(strings.NewReader(`{"result":{"config":{}}}`)),
			Request:    request,
		}, nil
	})
	defer func() { http.DefaultTransport = originalTransport }()

	directory := t.TempDir()
	configFile := filepath.Join(directory, "config.yaml")
	resumeFile := filepath.Join(directory, "resume.json")
	tokenFile := filepath.Join(directory, "control.token")
	target, _ := url.Parse("http://core.invalid")
	supervisor := newCoreSupervisor("unused", configFile, resumeFile, tokenFile, "127.0.0.1:9090", target)
	if err := supervisor.Prepare(); err != nil {
		t.Fatal(err)
	}
	input, err := os.ReadFile(configFile)
	if err != nil {
		t.Fatal(err)
	}
	status, _, _, err := supervisor.ApplyYAMLConfig(context.Background(), "Bearer managed-token", input)
	if err != nil {
		t.Fatal(err)
	}
	if status != http.StatusOK {
		t.Fatalf("status = %d", status)
	}
	persisted, err := os.ReadFile(configFile)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(persisted), "# 日志级别 info/debug") || !strings.Contains(string(persisted), "# 是否创建系统 TUN 设备") {
		t.Fatal("YAML comments were not preserved after apply")
	}
}

func TestManagedCoreRestartsAfterUnexpectedExit(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("shell helper is not available on Windows")
	}
	directory := t.TempDir()
	counterFile := filepath.Join(directory, "starts.txt")
	helper := filepath.Join(directory, "fake-core.sh")
	script := "#!/bin/sh\necho started >> " + counterFile + "\nexit 1\n"
	if err := os.WriteFile(helper, []byte(script), 0700); err != nil {
		t.Fatal(err)
	}
	target, _ := url.Parse("http://127.0.0.1:9090")
	supervisor := newCoreSupervisor(helper, filepath.Join(directory, "config.yaml"), filepath.Join(directory, "resume.json"), filepath.Join(directory, "control.token"), "127.0.0.1:9090", target)
	if err := supervisor.Start(); err != nil {
		t.Fatal(err)
	}
	deadline := time.Now().Add(4 * time.Second)
	for {
		data, _ := os.ReadFile(counterFile)
		if strings.Count(string(data), "started") >= 2 {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("Core was not restarted; starts=%q", data)
		}
		time.Sleep(50 * time.Millisecond)
	}
	stopCtx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	if err := supervisor.Stop(stopCtx); err != nil {
		t.Fatal(err)
	}
}

func assertManagedConfig(t *testing.T, config map[string]any, resumeFile, tokenFile string) {
	t.Helper()
	control := config["control"].(map[string]any)
	rest := control["rest"].(map[string]any)
	if rest["enabled"] != true || rest["listen"] != "127.0.0.1:9090" || rest["secret-file"] != tokenFile {
		t.Fatalf("managed REST config = %#v", rest)
	}
	if secret, exists := rest["secret"]; exists && secret != "" {
		t.Fatalf("managed REST secret was not cleared: %#v", rest)
	}
	state := config["state"].(map[string]any)
	if state["resume-file"] != resumeFile {
		t.Fatalf("managed Resume State = %#v", state)
	}
}
