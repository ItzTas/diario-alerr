//go:build !linux

package coretest

import "errors"

func startCluster() (string, func() error, error) {
	return "", nil, errors.New("a local Postgres for tests only starts on Linux; set TEST_DATABASE_URL")
}
