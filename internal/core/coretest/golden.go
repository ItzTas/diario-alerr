package coretest

import (
	"bytes"
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"testing"
)

const regenerateCommand = "go test ./internal/core ./internal/gazette ./internal/watch -run TestGoldenVectors -update-golden"

var updateGolden bool

func init() {
	flag.BoolVar(&updateGolden, "update-golden", false, "rewrite the files under testdata/golden")
}

func Golden(t testing.TB, name string, value any) {
	t.Helper()
	var out bytes.Buffer
	encoder := json.NewEncoder(&out)
	encoder.SetEscapeHTML(false)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(value); err != nil {
		t.Fatalf("encode golden %s: %v", name, err)
	}
	GoldenBytes(t, name, out.Bytes())
}

func GoldenBytes(t testing.TB, name string, data []byte) {
	t.Helper()
	path := filepath.Join(moduleRoot(t), "testdata", "golden", name)
	if updateGolden {
		if err := os.MkdirAll(filepath.Dir(path), 0o750); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, data, 0o600); err != nil {
			t.Fatal(err)
		}
		return
	}
	want, err := os.ReadFile(path)
	if err != nil {
		t.Errorf("read golden %s: %v\nregenerate with: %s", name, err, regenerateCommand)
		return
	}
	if !bytes.Equal(want, data) {
		t.Errorf("golden %s differs from the current output\nregenerate with: %s", name, regenerateCommand)
	}
}

func moduleRoot(t testing.TB) string {
	t.Helper()
	dir, err := os.Getwd()
	if err != nil {
		t.Fatal(err)
	}
	for {
		if _, err := os.Stat(filepath.Join(dir, "go.mod")); err == nil {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatal("no go.mod above the working directory")
		}
		dir = parent
	}
}
