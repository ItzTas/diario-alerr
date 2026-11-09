//go:build linux

package coretest

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"syscall"
	"time"
)

func startCluster() (string, func() error, error) {
	dir, err := os.MkdirTemp("", "diario-alerr-pg")
	if err != nil {
		return "", nil, err
	}
	data := filepath.Join(dir, "data")
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()
	initdb := exec.CommandContext(ctx, "initdb", "-D", data, "-U", "postgres", "-A", "trust", "-E", "UTF8", "--locale=C", "--no-sync", "--no-instructions")
	if output, err := initdb.CombinedOutput(); err != nil {
		return "", nil, errors.Join(fmt.Errorf("initdb: %w\n%s", err, output), os.RemoveAll(dir))
	}
	postgres := exec.CommandContext(context.Background(), "postgres", "-D", data, "-k", dir,
		"-c", "listen_addresses=", "-c", "fsync=off", "-c", "synchronous_commit=off", "-c", "full_page_writes=off")
	postgres.SysProcAttr = &syscall.SysProcAttr{Pdeathsig: syscall.SIGQUIT}
	exited, err := startTied(postgres)
	if err != nil {
		return "", nil, errors.Join(err, os.RemoveAll(dir))
	}
	return dir, func() error {
		return errors.Join(stopProcess(postgres, exited), os.RemoveAll(dir))
	}, nil
}

func startTied(cmd *exec.Cmd) (<-chan struct{}, error) {
	started := make(chan error, 1)
	exited := make(chan struct{})
	go func() {
		runtime.LockOSThread()
		if err := cmd.Start(); err != nil {
			started <- err
			return
		}
		started <- nil
		_ = cmd.Wait()
		close(exited)
	}()
	return exited, <-started
}

func stopProcess(cmd *exec.Cmd, exited <-chan struct{}) error {
	if err := cmd.Process.Signal(syscall.SIGINT); err != nil {
		return err
	}
	select {
	case <-exited:
		return nil
	case <-time.After(10 * time.Second):
		return cmd.Process.Kill()
	}
}
