package main

import (
	"testing"

	workerv1 "github.com/benediktms/beholder/workers/typescript/internal/proto/beholder/worker/v1"
)

func TestStorybookCandidate(t *testing.T) {
	t.Parallel()

	for _, path := range []string{
		".storybook/preview.js",
		"src/__stories__/button.tsx",
		"src/button.stories.tsx",
		"src/button.story.tsx",
	} {
		if !isStorybookCandidate(&workerv1.SemanticCandidate{Span: &workerv1.SourceSpan{Path: path}}) {
			t.Errorf("expected Storybook path: %s", path)
		}
	}
	if isStorybookCandidate(&workerv1.SemanticCandidate{Span: &workerv1.SourceSpan{Path: "src/button.tsx"}}) {
		t.Error("production source classified as Storybook")
	}
}
