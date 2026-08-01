package app

import (
	"context"
	"fmt"
	"log"
	"net"
	"strconv"
	"strings"
	"sync"
	"time"
)

const defaultSystemProxyGuardInterval = 5 * time.Second

type systemProxyEndpoint struct {
	Host    string
	Port    int
	Address string
}

type systemProxySettings struct {
	HTTP  *systemProxyEndpoint
	SOCKS *systemProxyEndpoint
}

type systemProxyState struct {
	Supported      bool   `json:"supported"`
	SOCKSSupported bool   `json:"socks_supported"`
	Enabled        bool   `json:"enabled"`
	HTTPAddress    string `json:"http_address,omitempty"`
	SOCKSAddress   string `json:"socks_address,omitempty"`
}

type systemProxyPlatform interface {
	Supported() bool
	SupportsSOCKS() bool
	Enable(context.Context, systemProxySettings) error
	Disable(context.Context) error
	Matches(context.Context, systemProxySettings) (bool, error)
}

type systemProxyController struct {
	mu            sync.Mutex
	platform      systemProxyPlatform
	enabled       bool
	httpAddress   string
	socksAddress  string
	settings      systemProxySettings
	guardInterval time.Duration
	guardStarted  bool
	guardStop     chan struct{}
	guardDone     chan struct{}
	guardError    string
}

func newSystemProxyController() *systemProxyController {
	return newSystemProxyControllerWithPlatform(newSystemProxyPlatform())
}

func newSystemProxyControllerWithPlatform(platform systemProxyPlatform) *systemProxyController {
	return &systemProxyController{platform: platform, guardInterval: defaultSystemProxyGuardInterval}
}

func (controller *systemProxyController) Status() systemProxyState {
	controller.mu.Lock()
	defer controller.mu.Unlock()
	return controller.stateLocked()
}

func (controller *systemProxyController) Configure(ctx context.Context, enabled bool, httpAddress, socksAddress string) (systemProxyState, error) {
	controller.mu.Lock()
	defer controller.mu.Unlock()
	if !controller.platform.Supported() {
		return controller.stateLocked(), fmt.Errorf("system proxy is not supported on this platform")
	}
	if !enabled {
		if err := controller.platform.Disable(ctx); err != nil {
			return controller.stateLocked(), err
		}
		controller.enabled = false
		controller.httpAddress = ""
		controller.socksAddress = ""
		controller.settings = systemProxySettings{}
		controller.guardError = ""
		return controller.stateLocked(), nil
	}
	settings := systemProxySettings{}
	if strings.TrimSpace(httpAddress) != "" {
		endpoint, err := parseSystemProxyEndpoint(httpAddress)
		if err != nil {
			return controller.stateLocked(), fmt.Errorf("invalid HTTP proxy address: %w", err)
		}
		settings.HTTP = &endpoint
	}
	if controller.platform.SupportsSOCKS() && strings.TrimSpace(socksAddress) != "" {
		endpoint, err := parseSystemProxyEndpoint(socksAddress)
		if err != nil {
			return controller.stateLocked(), fmt.Errorf("invalid SOCKS proxy address: %w", err)
		}
		settings.SOCKS = &endpoint
	}
	if settings.HTTP == nil && settings.SOCKS == nil {
		return controller.stateLocked(), fmt.Errorf("no supported active proxy inbound is available")
	}
	if err := controller.platform.Enable(ctx, settings); err != nil {
		return controller.stateLocked(), err
	}
	controller.enabled = true
	controller.settings = settings
	controller.httpAddress = endpointAddress(settings.HTTP)
	controller.socksAddress = endpointAddress(settings.SOCKS)
	controller.startGuardLocked()
	return controller.stateLocked(), nil
}

func (controller *systemProxyController) Close(ctx context.Context) error {
	controller.mu.Lock()
	var disableErr error
	if controller.enabled && controller.platform.Supported() {
		disableErr = controller.platform.Disable(ctx)
		if disableErr == nil {
			controller.enabled = false
			controller.httpAddress = ""
			controller.socksAddress = ""
			controller.settings = systemProxySettings{}
		}
	}
	var stop chan struct{}
	var done chan struct{}
	if controller.guardStarted {
		stop = controller.guardStop
		done = controller.guardDone
		controller.guardStarted = false
		controller.guardStop = nil
		controller.guardDone = nil
	}
	controller.mu.Unlock()
	if stop != nil {
		close(stop)
		<-done
	}
	return disableErr
}

func (controller *systemProxyController) startGuardLocked() {
	if controller.guardStarted {
		return
	}
	controller.guardStarted = true
	controller.guardStop = make(chan struct{})
	controller.guardDone = make(chan struct{})
	interval := controller.guardInterval
	go controller.guardLoop(interval, controller.guardStop, controller.guardDone)
}

func (controller *systemProxyController) guardLoop(interval time.Duration, stop <-chan struct{}, done chan<- struct{}) {
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	defer close(done)
	for {
		select {
		case <-ticker.C:
			controller.enforceSystemProxy()
		case <-stop:
			return
		}
	}
}

func (controller *systemProxyController) enforceSystemProxy() {
	controller.mu.Lock()
	defer controller.mu.Unlock()
	if !controller.enabled {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	matches, err := controller.platform.Matches(ctx, controller.settings)
	if err == nil && !matches {
		err = controller.platform.Enable(ctx, controller.settings)
		if err == nil {
			log.Printf("system proxy settings changed externally; reapplied")
		}
	}
	if err != nil {
		message := err.Error()
		if message != controller.guardError {
			log.Printf("enforce system proxy: %v", err)
			controller.guardError = message
		}
		return
	}
	controller.guardError = ""
}

func (controller *systemProxyController) stateLocked() systemProxyState {
	return systemProxyState{
		Supported:      controller.platform.Supported(),
		SOCKSSupported: controller.platform.SupportsSOCKS(),
		Enabled:        controller.enabled,
		HTTPAddress:    controller.httpAddress,
		SOCKSAddress:   controller.socksAddress,
	}
}

func endpointAddress(endpoint *systemProxyEndpoint) string {
	if endpoint == nil {
		return ""
	}
	return endpoint.Address
}

func parseSystemProxyEndpoint(address string) (systemProxyEndpoint, error) {
	host, portText, err := net.SplitHostPort(strings.TrimSpace(address))
	if err != nil {
		return systemProxyEndpoint{}, fmt.Errorf("invalid proxy listen address %q: %w", address, err)
	}
	if !isLoopbackHost(host) {
		return systemProxyEndpoint{}, fmt.Errorf("system proxy address must use a loopback host")
	}
	port, err := strconv.Atoi(portText)
	if err != nil || port < 1 || port > 65535 {
		return systemProxyEndpoint{}, fmt.Errorf("invalid proxy port %q", portText)
	}
	return systemProxyEndpoint{Host: host, Port: port, Address: net.JoinHostPort(host, strconv.Itoa(port))}, nil
}

func parseSystemProxyAddress(address string) (string, int, string, error) {
	endpoint, err := parseSystemProxyEndpoint(address)
	if err != nil {
		return "", 0, "", err
	}
	return endpoint.Host, endpoint.Port, endpoint.Address, nil
}

func isLoopbackHost(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	address := net.ParseIP(host)
	return address != nil && address.IsLoopback()
}
