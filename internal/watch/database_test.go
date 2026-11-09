package watch

import (
	"os"
	"testing"

	"github.com/ItzTas/diario-alerr/internal/core/coretest"
)

func TestMain(m *testing.M) {
	os.Exit(coretest.Run(m))
}
