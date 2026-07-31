//go:build !windows

package main

import (
	"fmt"
	"os"
	"os/exec"
)

func tunPrivilegesAvailable() bool {
	return os.Geteuid() == 0
}

func validateTUNPrivileges() error {
	if !tunPrivilegesAvailable() {
		return fmt.Errorf("TUN is enabled but Sumire is not running as root; restart with sudo ./sumire")
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
