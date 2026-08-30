package main

import (
	"path/filepath"
	"strings"

	workerv1 "github.com/benediktms/beholder/workers/typescript/internal/proto/beholder/worker/v1"
)

func isStorybookCandidate(candidate *workerv1.SemanticCandidate) bool {
	path := "/" + filepath.ToSlash(candidate.GetSpan().GetPath())
	name := filepath.Base(path)
	return strings.Contains(path, "/.storybook/") ||
		strings.Contains(path, "/__stories__/") ||
		strings.Contains(name, ".stories.") ||
		strings.Contains(name, ".story.")
}
