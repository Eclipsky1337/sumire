//go:build windows

package app

import (
	"context"
	"fmt"
	"net"
	"strconv"

	"golang.org/x/sys/windows"
	"golang.org/x/sys/windows/registry"
)

const (
	windowsInternetSettingsKey         = `Software\Microsoft\Windows\CurrentVersion\Internet Settings`
	internetOptionRefresh              = 37
	internetOptionProxySettingsChanged = 95
)

var internetSetOption = windows.NewLazySystemDLL("wininet.dll").NewProc("InternetSetOptionW")

type windowsSystemProxy struct{}

func newSystemProxyPlatform() systemProxyPlatform {
	return windowsSystemProxy{}
}

func (windowsSystemProxy) Supported() bool {
	return true
}

func (windowsSystemProxy) SupportsSOCKS() bool {
	return false
}

func (windowsSystemProxy) Enable(_ context.Context, settings systemProxySettings) error {
	if settings.HTTP == nil {
		return fmt.Errorf("Windows system proxy requires an active HTTP inbound")
	}
	address := net.JoinHostPort(settings.HTTP.Host, strconv.Itoa(settings.HTTP.Port))
	key, err := registry.OpenKey(registry.CURRENT_USER, windowsInternetSettingsKey, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("open Windows internet settings: %w", err)
	}
	defer key.Close()
	if err := key.SetStringValue("ProxyServer", "http="+address+";https="+address); err != nil {
		return fmt.Errorf("set Windows proxy server: %w", err)
	}
	if err := key.SetStringValue("ProxyOverride", "<local>;localhost;127.*;[::1]"); err != nil {
		return fmt.Errorf("set Windows proxy bypass: %w", err)
	}
	if err := key.SetDWordValue("ProxyEnable", 1); err != nil {
		return fmt.Errorf("enable Windows proxy: %w", err)
	}
	return notifyWindowsProxyChanged()
}

func (windowsSystemProxy) Disable(_ context.Context) error {
	key, err := registry.OpenKey(registry.CURRENT_USER, windowsInternetSettingsKey, registry.SET_VALUE)
	if err != nil {
		return fmt.Errorf("open Windows internet settings: %w", err)
	}
	defer key.Close()
	if err := key.SetDWordValue("ProxyEnable", 0); err != nil {
		return fmt.Errorf("disable Windows proxy: %w", err)
	}
	if err := key.SetStringValue("ProxyServer", ""); err != nil {
		return fmt.Errorf("clear Windows proxy server: %w", err)
	}
	return notifyWindowsProxyChanged()
}

func (windowsSystemProxy) Matches(_ context.Context, settings systemProxySettings) (bool, error) {
	if settings.HTTP == nil {
		return false, nil
	}
	key, err := registry.OpenKey(registry.CURRENT_USER, windowsInternetSettingsKey, registry.QUERY_VALUE)
	if err != nil {
		return false, fmt.Errorf("open Windows internet settings: %w", err)
	}
	defer key.Close()
	enabled, _, err := key.GetIntegerValue("ProxyEnable")
	if err != nil || enabled != 1 {
		return false, nil
	}
	proxyServer, _, err := key.GetStringValue("ProxyServer")
	if err != nil {
		return false, nil
	}
	proxyOverride, _, err := key.GetStringValue("ProxyOverride")
	if err != nil {
		return false, nil
	}
	address := net.JoinHostPort(settings.HTTP.Host, strconv.Itoa(settings.HTTP.Port))
	return proxyServer == "http="+address+";https="+address && proxyOverride == "<local>;localhost;127.*;[::1]", nil
}

func notifyWindowsProxyChanged() error {
	if result, _, callErr := internetSetOption.Call(0, internetOptionProxySettingsChanged, 0, 0); result == 0 {
		return fmt.Errorf("notify Windows proxy settings change: %w", callErr)
	}
	if result, _, callErr := internetSetOption.Call(0, internetOptionRefresh, 0, 0); result == 0 {
		return fmt.Errorf("refresh Windows internet settings: %w", callErr)
	}
	return nil
}
