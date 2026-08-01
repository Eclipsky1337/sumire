//go:build !windows

package main

import "testing"

func TestSudoInvokingOwner(t *testing.T) {
	tests := []struct {
		name         string
		effectiveUID int
		sudoUID      string
		sudoGID      string
		uid          int
		gid          int
		ok           bool
	}{
		{name: "sudo user", effectiveUID: 0, sudoUID: "501", sudoGID: "20", uid: 501, gid: 20, ok: true},
		{name: "ordinary process", effectiveUID: 501, sudoUID: "501", sudoGID: "20"},
		{name: "missing environment", effectiveUID: 0},
		{name: "invalid uid", effectiveUID: 0, sudoUID: "invalid", sudoGID: "20"},
		{name: "negative gid", effectiveUID: 0, sudoUID: "501", sudoGID: "-1"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			uid, gid, ok := sudoInvokingOwner(test.effectiveUID, test.sudoUID, test.sudoGID)
			if uid != test.uid || gid != test.gid || ok != test.ok {
				t.Fatalf("owner = (%d, %d, %t)", uid, gid, ok)
			}
		})
	}
}
