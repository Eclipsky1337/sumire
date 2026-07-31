//go:build darwin

package main

import (
	"context"
	"fmt"
	"os/exec"
	"strconv"
	"strings"
)

const networkSetupPath = "/usr/sbin/networksetup"

type darwinSystemProxy struct{}

func newSystemProxyPlatform() systemProxyPlatform {
	return darwinSystemProxy{}
}

func (darwinSystemProxy) Supported() bool {
	return true
}

func (darwinSystemProxy) SupportsSOCKS() bool {
	return true
}

func (darwinSystemProxy) Enable(ctx context.Context, settings systemProxySettings) error {
	services, err := darwinNetworkServices(ctx)
	if err != nil {
		return err
	}
	for _, service := range services {
		commands := make([][]string, 0, 7)
		if settings.HTTP != nil {
			portText := strconv.Itoa(settings.HTTP.Port)
			commands = append(commands,
				[]string{"-setwebproxy", service, settings.HTTP.Host, portText, "off"},
				[]string{"-setwebproxystate", service, "on"},
				[]string{"-setsecurewebproxy", service, settings.HTTP.Host, portText, "off"},
				[]string{"-setsecurewebproxystate", service, "on"},
			)
		} else {
			commands = append(commands,
				[]string{"-setwebproxystate", service, "off"},
				[]string{"-setsecurewebproxystate", service, "off"},
			)
		}
		if settings.SOCKS != nil {
			commands = append(commands,
				[]string{"-setsocksfirewallproxy", service, settings.SOCKS.Host, strconv.Itoa(settings.SOCKS.Port), "off"},
				[]string{"-setsocksfirewallproxystate", service, "on"},
			)
		} else {
			commands = append(commands, []string{"-setsocksfirewallproxystate", service, "off"})
		}
		commands = append(commands, []string{"-setproxybypassdomains", service, "localhost", "127.0.0.1", "::1"})
		for _, arguments := range commands {
			if err := runNetworkSetup(ctx, arguments...); err != nil {
				return fmt.Errorf("configure network service %q: %w", service, err)
			}
		}
	}
	return nil
}

func (darwinSystemProxy) Disable(ctx context.Context) error {
	services, err := darwinNetworkServices(ctx)
	if err != nil {
		return err
	}
	for _, service := range services {
		for _, arguments := range [][]string{{"-setwebproxystate", service, "off"}, {"-setsecurewebproxystate", service, "off"}, {"-setsocksfirewallproxystate", service, "off"}} {
			if err := runNetworkSetup(ctx, arguments...); err != nil {
				return fmt.Errorf("disable proxy for network service %q: %w", service, err)
			}
		}
	}
	return nil
}

func (darwinSystemProxy) Matches(ctx context.Context, settings systemProxySettings) (bool, error) {
	services, err := darwinNetworkServices(ctx)
	if err != nil {
		return false, err
	}
	for _, service := range services {
		checks := []struct {
			command  string
			endpoint *systemProxyEndpoint
		}{
			{"-getwebproxy", settings.HTTP},
			{"-getsecurewebproxy", settings.HTTP},
			{"-getsocksfirewallproxy", settings.SOCKS},
		}
		for _, check := range checks {
			matches, err := darwinProxyMatches(ctx, service, check.command, check.endpoint)
			if err != nil {
				return false, err
			}
			if !matches {
				return false, nil
			}
		}
	}
	return true, nil
}

func darwinProxyMatches(ctx context.Context, service, command string, expected *systemProxyEndpoint) (bool, error) {
	output, err := runNetworkSetupOutput(ctx, command, service)
	if err != nil {
		return false, fmt.Errorf("read proxy for network service %q: %w", service, err)
	}
	values := make(map[string]string)
	for _, line := range strings.Split(string(output), "\n") {
		key, value, found := strings.Cut(line, ":")
		if found {
			values[strings.TrimSpace(key)] = strings.TrimSpace(value)
		}
	}
	enabled := strings.EqualFold(values["Enabled"], "Yes")
	if expected == nil {
		return !enabled, nil
	}
	port, err := strconv.Atoi(values["Port"])
	if err != nil {
		return false, nil
	}
	return enabled && strings.EqualFold(values["Server"], expected.Host) && port == expected.Port, nil
}

func darwinNetworkServices(ctx context.Context) ([]string, error) {
	command := exec.CommandContext(ctx, networkSetupPath, "-listallnetworkservices")
	output, err := command.CombinedOutput()
	if err != nil {
		return nil, fmt.Errorf("list macOS network services: %s: %w", strings.TrimSpace(string(output)), err)
	}
	var services []string
	for _, line := range strings.Split(string(output), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "An asterisk") || strings.HasPrefix(line, "*") {
			continue
		}
		services = append(services, line)
	}
	if len(services) == 0 {
		return nil, fmt.Errorf("no enabled macOS network services found")
	}
	return services, nil
}

func runNetworkSetup(ctx context.Context, arguments ...string) error {
	_, err := runNetworkSetupOutput(ctx, arguments...)
	return err
}

func runNetworkSetupOutput(ctx context.Context, arguments ...string) ([]byte, error) {
	command := exec.CommandContext(ctx, networkSetupPath, arguments...)
	output, err := command.CombinedOutput()
	if err != nil {
		return nil, fmt.Errorf("networksetup %s: %s: %w", strings.Join(arguments, " "), strings.TrimSpace(string(output)), err)
	}
	return output, nil
}
