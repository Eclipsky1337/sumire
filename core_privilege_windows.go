//go:build windows

package main

import (
	"fmt"
	"os/exec"

	"golang.org/x/sys/windows"
)

func tunPrivilegesAvailable() bool {
	return windows.GetCurrentProcessToken().IsElevated()
}

func validateTUNPrivileges() error {
	if !tunPrivilegesAvailable() {
		return fmt.Errorf("TUN is enabled but Sumire is not running as administrator")
	}
	return nil
}

func newManagedCoreCommand(binary, configFile string, tunEnabled bool) (*exec.Cmd, bool, error) {
	if tunEnabled {
		if err := validateTUNPrivileges(); err != nil {
			return nil, false, err
		}
	}
	return exec.Command(binary, "--config", configFile), tunEnabled, nil
}
