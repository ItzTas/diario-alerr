package gazette

import (
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"

	"github.com/ItzTas/diario-alerr/internal/core"
)

const pdfTimeout = 5 * time.Minute

func (c *Client) downloadPages(ctx context.Context, pdfURL string) ([]string, error) {
	resp, err := core.Get(ctx, c.HTTP, pdfURL)
	if err != nil {
		return nil, err
	}
	defer func() { _ = resp.Body.Close() }()
	file, err := os.CreateTemp("", "edition-*.pdf")
	if err != nil {
		return nil, fmt.Errorf("create temp file: %w", err)
	}
	defer func() { _ = os.Remove(file.Name()) }()
	_, copyErr := io.Copy(file, resp.Body)
	closeErr := file.Close()
	if copyErr != nil {
		return nil, fmt.Errorf("download %s: %w", pdfURL, copyErr)
	}
	if closeErr != nil {
		return nil, fmt.Errorf("write %s: %w", file.Name(), closeErr)
	}
	return extractPages(ctx, file.Name())
}

func extractPages(ctx context.Context, path string) ([]string, error) {
	ctx, cancel := context.WithTimeout(ctx, pdfTimeout)
	defer cancel()
	out, err := exec.CommandContext(ctx, "pdftotext", "-enc", "UTF-8", path, "-").Output()
	if err != nil {
		return nil, fmt.Errorf("pdftotext: %w", err)
	}
	return strings.Split(string(out), "\f"), nil
}
