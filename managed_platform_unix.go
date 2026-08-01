//go:build !windows

package main

import (
	"fmt"
	"os"
	"os/exec"
	"strconv"
	"syscall"
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

func sudoInvokingOwner(effectiveUID int, sudoUID, sudoGID string) (int, int, bool) {
	if effectiveUID != 0 || sudoUID == "" || sudoGID == "" {
		return 0, 0, false
	}
	uid, uidErr := strconv.Atoi(sudoUID)
	gid, gidErr := strconv.Atoi(sudoGID)
	if uidErr != nil || gidErr != nil || uid < 0 || gid < 0 {
		return 0, 0, false
	}
	return uid, gid, true
}

func invokingManagedOwner() (int, int, bool) {
	return sudoInvokingOwner(os.Geteuid(), os.Getenv("SUDO_UID"), os.Getenv("SUDO_GID"))
}

func applyManagedPathOwnership(path string) error {
	uid, gid, ok := invokingManagedOwner()
	if !ok {
		return nil
	}
	return os.Chown(path, uid, gid)
}

func applyManagedFileOwnership(file *os.File, existing os.FileInfo) error {
	if uid, gid, ok := invokingManagedOwner(); ok {
		return file.Chown(uid, gid)
	}
	if existing == nil {
		return nil
	}
	stat, ok := existing.Sys().(*syscall.Stat_t)
	if !ok {
		return nil
	}
	return file.Chown(int(stat.Uid), int(stat.Gid))
}
