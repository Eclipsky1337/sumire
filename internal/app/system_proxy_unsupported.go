//go:build !darwin && !windows

package app

import (
	"context"
	"fmt"
)

type unsupportedSystemProxy struct{}

func newSystemProxyPlatform() systemProxyPlatform {
	return unsupportedSystemProxy{}
}

func (unsupportedSystemProxy) Supported() bool {
	return false
}

func (unsupportedSystemProxy) SupportsSOCKS() bool {
	return false
}

func (unsupportedSystemProxy) Enable(context.Context, systemProxySettings) error {
	return fmt.Errorf("system proxy is not supported on this platform")
}

func (unsupportedSystemProxy) Disable(context.Context) error {
	return fmt.Errorf("system proxy is not supported on this platform")
}

func (unsupportedSystemProxy) Matches(context.Context, systemProxySettings) (bool, error) {
	return false, fmt.Errorf("system proxy is not supported on this platform")
}
