package main

import (
	"context"
	"errors"
	"sync"
	"testing"
	"time"
)

type fakeSystemProxyPlatform struct {
	mu            sync.Mutex
	supported     bool
	supportsSOCKS bool
	settings      systemProxySettings
	matches       bool
	enableCalls   int
	disableCalls  int
	enableErr     error
	disableErr    error
}

func (platform *fakeSystemProxyPlatform) Supported() bool {
	return platform.supported
}

func (platform *fakeSystemProxyPlatform) SupportsSOCKS() bool {
	return platform.supportsSOCKS
}

func (platform *fakeSystemProxyPlatform) Enable(_ context.Context, settings systemProxySettings) error {
	platform.mu.Lock()
	defer platform.mu.Unlock()
	if platform.enableErr != nil {
		return platform.enableErr
	}
	platform.settings = settings
	platform.enableCalls++
	return nil
}

func (platform *fakeSystemProxyPlatform) Disable(context.Context) error {
	platform.mu.Lock()
	defer platform.mu.Unlock()
	platform.disableCalls++
	return platform.disableErr
}

func (platform *fakeSystemProxyPlatform) Matches(context.Context, systemProxySettings) (bool, error) {
	platform.mu.Lock()
	defer platform.mu.Unlock()
	return platform.matches, nil
}

func (platform *fakeSystemProxyPlatform) snapshot() (systemProxySettings, int, int) {
	platform.mu.Lock()
	defer platform.mu.Unlock()
	return platform.settings, platform.enableCalls, platform.disableCalls
}

func (platform *fakeSystemProxyPlatform) setMatches(matches bool) {
	platform.mu.Lock()
	defer platform.mu.Unlock()
	platform.matches = matches
}

func TestParseSystemProxyAddress(t *testing.T) {
	for _, testCase := range []struct {
		address string
		host    string
		port    int
	}{
		{"127.0.0.1:1081", "127.0.0.1", 1081},
		{"localhost:8080", "localhost", 8080},
		{"[::1]:1081", "::1", 1081},
	} {
		host, port, normalized, err := parseSystemProxyAddress(testCase.address)
		if err != nil {
			t.Fatalf("parse %q: %v", testCase.address, err)
		}
		if host != testCase.host || port != testCase.port || normalized != testCase.address {
			t.Fatalf("parse %q = %q, %d, %q", testCase.address, host, port, normalized)
		}
	}
	for _, address := range []string{"0.0.0.0:1081", "192.168.1.2:1081", "127.0.0.1:0", "invalid"} {
		if _, _, _, err := parseSystemProxyAddress(address); err == nil {
			t.Fatalf("expected %q to be rejected", address)
		}
	}
}

func TestSystemProxyControllerConfigureAndClose(t *testing.T) {
	platform := &fakeSystemProxyPlatform{supported: true, supportsSOCKS: true}
	controller := newSystemProxyControllerWithPlatform(platform)
	state, err := controller.Configure(context.Background(), true, "127.0.0.1:1081", "127.0.0.1:1080")
	if err != nil {
		t.Fatal(err)
	}
	settings, _, _ := platform.snapshot()
	if !state.Enabled || state.HTTPAddress != "127.0.0.1:1081" || state.SOCKSAddress != "127.0.0.1:1080" || settings.HTTP == nil || settings.SOCKS == nil {
		t.Fatalf("state = %+v, platform = %+v", state, platform)
	}
	if err := controller.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
	_, _, disableCalls := platform.snapshot()
	if disableCalls != 1 || controller.Status().Enabled {
		t.Fatalf("disable calls = %d, state = %+v", disableCalls, controller.Status())
	}
}

func TestSystemProxyControllerKeepsStateWhenDisableFails(t *testing.T) {
	platform := &fakeSystemProxyPlatform{supported: true, disableErr: errors.New("disable failed")}
	controller := newSystemProxyControllerWithPlatform(platform)
	if _, err := controller.Configure(context.Background(), true, "127.0.0.1:1081", ""); err != nil {
		t.Fatal(err)
	}
	if _, err := controller.Configure(context.Background(), false, "", ""); err == nil {
		t.Fatal("expected disable failure")
	}
	if !controller.Status().Enabled {
		t.Fatal("controller forgot enabled state after disable failure")
	}
	platform.disableErr = nil
	if err := controller.Close(context.Background()); err != nil {
		t.Fatal(err)
	}
}

func TestUnsupportedSystemProxyController(t *testing.T) {
	controller := newSystemProxyControllerWithPlatform(&fakeSystemProxyPlatform{})
	if state := controller.Status(); state.Supported || state.Enabled {
		t.Fatalf("state = %+v", state)
	}
	if _, err := controller.Configure(context.Background(), true, "127.0.0.1:1081", ""); err == nil {
		t.Fatal("expected unsupported platform error")
	}
}

func TestSystemProxyGuardReappliesChangedSettings(t *testing.T) {
	platform := &fakeSystemProxyPlatform{supported: true, supportsSOCKS: true}
	controller := newSystemProxyControllerWithPlatform(platform)
	controller.guardInterval = 10 * time.Millisecond
	if _, err := controller.Configure(context.Background(), true, "127.0.0.1:1081", "127.0.0.1:1080"); err != nil {
		t.Fatal(err)
	}
	defer func() {
		platform.disableErr = nil
		if err := controller.Close(context.Background()); err != nil {
			t.Fatal(err)
		}
	}()
	deadline := time.Now().Add(time.Second)
	for {
		_, enableCalls, _ := platform.snapshot()
		if enableCalls >= 2 {
			platform.setMatches(true)
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("guard did not reapply settings; enable calls = %d", enableCalls)
		}
		time.Sleep(5 * time.Millisecond)
	}
}
