package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	workerv1 "github.com/benediktms/beholder/workers/typescript/internal/proto/beholder/worker/v1"
)

func TestDefinitionUsesStandardLSP(t *testing.T) {
	if os.Getenv("BEHOLDER_TYPESCRIPT_LSP_HELPER") == "1" {
		runLSPHelper()
		os.Exit(0)
	}

	root := t.TempDir()
	source := filepath.Join(root, "src", "caller.ts")
	if err := os.MkdirAll(filepath.Dir(source), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(source, []byte("target()\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Setenv("BEHOLDER_TYPESCRIPT_LSP_HELPER", "1")
	t.Setenv("BEHOLDER_TYPESCRIPT_EXPECT_MEMORY_LIMIT", "4GiB")
	t.Setenv("BEHOLDER_TYPESCRIPT_NOTIFICATION_BURST", "65")
	t.Setenv("BEHOLDER_TYPESCRIPT_SERVER_REQUEST", "1")
	t.Setenv("GOMEMLIMIT", "")
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	c, err := startClient(ctx, os.Args[0], root, "-test.run=TestDefinitionUsesStandardLSP")
	if err != nil {
		t.Fatal(err)
	}
	defer c.close()
	if err := c.initialize(ctx, root); err != nil {
		t.Fatal(err)
	}
	definitions, err := c.definitions(ctx, source, position{Line: 0, Character: 1})
	if err != nil {
		t.Fatal(err)
	}
	if len(definitions) != 1 {
		t.Fatalf("unexpected definitions: %+v", definitions)
	}
	definition := definitions[0]
	if definition.URI != "file:///repo/src/target.ts" || definition.Range.Start.Line != 2 {
		t.Fatalf("unexpected definition: %+v", definition)
	}
}

func TestAnalyzeSnapshotPublishesExactCandidateOverride(t *testing.T) {
	root := t.TempDir()
	caller := "const counter = new Counter(); counter.value();\n"
	target := "export class Counter { value() {} }\n"
	unrelated := "export const unused = true;\n"
	writeTestFile(t, root, "src/caller.ts", caller, 0o644)
	writeTestFile(t, root, "src/target.ts", target, 0o644)
	writeTestFile(t, root, "src/unrelated.ts", unrelated, 0o644)
	t.Setenv("BEHOLDER_TYPESCRIPT_FORBIDDEN_URI", fileURI(filepath.Join(root, "src", "unrelated.ts")))
	helpTarget := fileURI(filepath.Join(root, "src", "target.ts"))
	script := fmt.Sprintf("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Version 7.0.2'; else BEHOLDER_TYPESCRIPT_LSP_HELPER=1 BEHOLDER_TYPESCRIPT_TARGET_URI='%s' exec '%s' -test.run=TestDefinitionUsesStandardLSP; fi\n", helpTarget, strings.ReplaceAll(os.Args[0], "'", "'\\''"))
	writeTestFile(t, root, "node_modules/.bin/tsc", script, 0o755)
	start := uint32(strings.Index(caller, "value"))
	snapshot := &analysisSnapshot{
		workspace: "test",
		repositories: map[string]*repositorySnapshot{"example": {
			identity: "example", base: root, target: true,
			inputs: map[string][]byte{"src/caller.ts": []byte(caller), "src/target.ts": []byte(target), "src/unrelated.ts": []byte(unrelated)},
		}},
		entities: map[string]bool{"repo://example/typescript/src/target/Counter/value": true},
		candidates: []*workerv1.SemanticCandidate{{
			Id: "candidate", Repository: "example", From: "repo://example/typescript/src/caller",
			UnresolvedTo: "typescript-method://counter/value", Span: &workerv1.SourceSpan{Path: "src/caller.ts", Start: &workerv1.SourcePosition{Line: 0, Character: start}, End: &workerv1.SourcePosition{Line: 0, Character: start + 5}},
		}},
	}

	result := analyzeSnapshot(context.Background(), snapshot, snapshot.repositories["example"], nil)

	if len(result.diagnostics) != 0 || len(result.overrides) != 1 {
		t.Fatalf("unexpected result: %+v", result)
	}
	if result.overrides[0].GetResolvedTo() != "repo://example/typescript/src/target/Counter/value" {
		t.Fatalf("unexpected override: %+v", result.overrides[0])
	}
	if result.compilerVersion != "7.0.2" || !strings.Contains(result.overrides[0].GetEvidence(), "src/target.ts:1") {
		t.Fatalf("unexpected compiler evidence: %+v", result)
	}
}

func TestAnalyzeSnapshotReportsCompilerFailureWithoutContribution(t *testing.T) {
	root := t.TempDir()
	source := "api.send();\n"
	writeTestFile(t, root, "src/caller.ts", source, 0o644)
	snapshot := &analysisSnapshot{
		workspace: "test",
		repositories: map[string]*repositorySnapshot{"example": {
			identity: "example", base: root, target: true,
			inputs: map[string][]byte{"src/caller.ts": []byte(source)},
		}},
		entities: map[string]bool{},
		candidates: []*workerv1.SemanticCandidate{{
			Id: "candidate", Repository: "example", Span: &workerv1.SourceSpan{Path: "src/caller.ts", Start: &workerv1.SourcePosition{}},
		}},
	}

	result := analyzeSnapshot(context.Background(), snapshot, snapshot.repositories["example"], nil)

	if result.failureCode != "typescript.compiler.unavailable" || len(result.overrides) != 0 {
		t.Fatalf("unexpected failure result: %+v", result)
	}
}

func TestAnalyzeSnapshotMapsDefinitionIntoContextRepository(t *testing.T) {
	consumer := t.TempDir()
	library := t.TempDir()
	caller := "const counter = new Counter(); counter.value();\n"
	target := "export class Counter { value() {} }\n"
	writeTestFile(t, consumer, "src/caller.ts", caller, 0o644)
	writeTestFile(t, library, "src/target.ts", target, 0o644)
	script := fmt.Sprintf("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Version 7.0.2'; else BEHOLDER_TYPESCRIPT_LSP_HELPER=1 BEHOLDER_TYPESCRIPT_TARGET_URI='%s' exec '%s' -test.run=TestDefinitionUsesStandardLSP; fi\n", fileURI(filepath.Join(library, "src", "target.ts")), strings.ReplaceAll(os.Args[0], "'", "'\\''"))
	writeTestFile(t, consumer, "node_modules/.bin/tsc", script, 0o755)
	start := uint32(strings.Index(caller, "value"))
	snapshot := &analysisSnapshot{
		workspace: "test",
		repositories: map[string]*repositorySnapshot{
			"consumer": {identity: "consumer", base: consumer, target: true, inputs: map[string][]byte{"src/caller.ts": []byte(caller)}},
			"library":  {identity: "library", base: library, inputs: map[string][]byte{"src/target.ts": []byte(target)}},
		},
		entities: map[string]bool{"repo://library/typescript/src/target/Counter/value": true},
		candidates: []*workerv1.SemanticCandidate{{
			Id: "candidate", Repository: "consumer", From: "repo://consumer/typescript/src/caller",
			UnresolvedTo: "typescript-method://counter/value", Span: &workerv1.SourceSpan{Path: "src/caller.ts", Start: &workerv1.SourcePosition{Character: start}, End: &workerv1.SourcePosition{Character: start + 5}},
		}},
	}

	result := analyzeSnapshot(context.Background(), snapshot, snapshot.repositories["consumer"], nil)

	if len(result.overrides) != 1 || result.overrides[0].GetResolvedTo() != "repo://library/typescript/src/target/Counter/value" {
		t.Fatalf("unexpected context override: %+v", result)
	}
}

func TestAnalyzeSnapshotTreatsCompilerCrashAsFailure(t *testing.T) {
	root := t.TempDir()
	caller := "const counter = new Counter(); counter.value();\n"
	target := "export class Counter { value() {} }\n"
	writeTestFile(t, root, "src/caller.ts", caller, 0o644)
	writeTestFile(t, root, "src/target.ts", target, 0o644)
	script := fmt.Sprintf("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'Version 7.0.2'; else BEHOLDER_TYPESCRIPT_LSP_HELPER=1 BEHOLDER_TYPESCRIPT_LSP_EXIT=1 exec '%s' -test.run=TestDefinitionUsesStandardLSP; fi\n", strings.ReplaceAll(os.Args[0], "'", "'\\''"))
	writeTestFile(t, root, "node_modules/.bin/tsc", script, 0o755)
	start := uint32(strings.Index(caller, "value"))
	snapshot := &analysisSnapshot{
		workspace: "test",
		repositories: map[string]*repositorySnapshot{"example": {
			identity: "example", base: root, target: true,
			inputs: map[string][]byte{"src/caller.ts": []byte(caller), "src/target.ts": []byte(target)},
		}},
		entities: map[string]bool{"repo://example/typescript/src/target/Counter/value": true},
		candidates: []*workerv1.SemanticCandidate{{
			Id: "candidate", Repository: "example", Span: &workerv1.SourceSpan{Path: "src/caller.ts", Start: &workerv1.SourcePosition{Character: start}},
		}},
	}

	result := analyzeSnapshot(context.Background(), snapshot, snapshot.repositories["example"], nil)

	if result.failureCode != "typescript.compiler.request_failed" || len(result.overrides) != 0 {
		t.Fatalf("unexpected crash result: %+v", result)
	}
}

func TestCompilerCandidateLimitIsDeterministic(t *testing.T) {
	candidates := make([]*workerv1.SemanticCandidate, maxCompilerCandidates+5)
	for index := range candidates {
		candidates[index] = &workerv1.SemanticCandidate{
			Id:           fmt.Sprintf("%04d", maxCompilerCandidates-index),
			UnresolvedTo: "typescript-constructor://Example",
		}
	}
	candidates[0] = &workerv1.SemanticCandidate{Id: "method", UnresolvedTo: "typescript-method://value/get"}
	candidates[1] = &workerv1.SemanticCandidate{Id: "z-direct", UnresolvedTo: "typescript-call://later"}
	candidates[2] = &workerv1.SemanticCandidate{Id: "a-direct", UnresolvedTo: "typescript-call://first"}
	candidates[3] = &workerv1.SemanticCandidate{Id: "javascript", UnresolvedTo: "javascript-call://best"}
	candidates[4] = &workerv1.SemanticCandidate{
		Id:           "storybook",
		UnresolvedTo: "javascript-call://noise",
		Span:         &workerv1.SourceSpan{Path: ".storybook/preview.js"},
	}

	candidates, skipped := boundedCandidates(candidates)

	if skipped != 5 || candidates[0].GetId() != "javascript" || candidates[1].GetId() != "a-direct" || candidates[2].GetId() != "z-direct" || candidates[3].GetId() != "method" {
		t.Fatalf("unexpected bounded candidates: %s..%s", candidates[0].GetId(), candidates[len(candidates)-1].GetId())
	}
	for _, candidate := range candidates {
		if candidate.GetId() == "storybook" {
			t.Fatal("Storybook candidate displaced a production candidate")
		}
	}
}

func writeTestFile(t *testing.T, root, path, content string, mode os.FileMode) {
	t.Helper()
	path = filepath.Join(root, filepath.FromSlash(path))
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), mode); err != nil {
		t.Fatal(err)
	}
}

func runLSPHelper() {
	if expected := os.Getenv("BEHOLDER_TYPESCRIPT_EXPECT_MEMORY_LIMIT"); expected != "" && os.Getenv("GOMEMLIMIT") != expected {
		return
	}
	reader := bufio.NewReader(os.Stdin)
	for {
		message, err := readHelperMessage(reader)
		if err != nil {
			return
		}
		switch message.Method {
		case "initialize":
			count, _ := strconv.Atoi(os.Getenv("BEHOLDER_TYPESCRIPT_NOTIFICATION_BURST"))
			for range count {
				writeHelperMessage(map[string]any{
					"jsonrpc": "2.0", "method": "window/logMessage", "params": map[string]string{"message": "loading"},
				})
			}
			if os.Getenv("BEHOLDER_TYPESCRIPT_SERVER_REQUEST") == "1" {
				writeHelperMessage(map[string]any{
					"jsonrpc": "2.0", "id": 99, "method": "workspace/configuration", "params": map[string]any{},
				})
				response, err := readHelperMessage(reader)
				if err != nil || string(response.ID) != "99" {
					return
				}
			}
			writeHelperMessage(map[string]any{
				"jsonrpc": "2.0", "id": message.ID,
				"result": map[string]any{"capabilities": map[string]any{}, "serverInfo": map[string]string{"name": "typescript-go", "version": "7.0.2"}},
			})
		case "textDocument/definition":
			if os.Getenv("BEHOLDER_TYPESCRIPT_LSP_EXIT") == "1" {
				return
			}
			target := os.Getenv("BEHOLDER_TYPESCRIPT_TARGET_URI")
			startLine, startCharacter, endCharacter := 0, 23, 28
			if target == "" {
				target = "file:///repo/src/target.ts"
				startLine, startCharacter, endCharacter = 2, 4, 10
			}
			writeHelperMessage(map[string]any{
				"jsonrpc": "2.0", "id": message.ID,
				"result": []map[string]any{{
					"targetUri": target,
					"targetSelectionRange": map[string]any{
						"start": map[string]int{"line": startLine, "character": startCharacter},
						"end":   map[string]int{"line": startLine, "character": endCharacter},
					},
				}},
			})
		case "textDocument/didOpen":
			var params struct {
				TextDocument struct {
					URI string `json:"uri"`
				} `json:"textDocument"`
			}
			if json.Unmarshal(message.Params, &params) != nil || params.TextDocument.URI == os.Getenv("BEHOLDER_TYPESCRIPT_FORBIDDEN_URI") {
				return
			}
		case "textDocument/documentSymbol":
			writeHelperMessage(map[string]any{
				"jsonrpc": "2.0", "id": message.ID,
				"result": []map[string]any{{
					"name": "Counter", "range": testRange(0, 7, 0, 37), "selectionRange": testRange(0, 13, 0, 20),
					"children": []map[string]any{{"name": "value", "range": testRange(0, 23, 0, 33), "selectionRange": testRange(0, 23, 0, 28)}},
				}},
			})
		case "shutdown":
			writeHelperMessage(map[string]any{"jsonrpc": "2.0", "id": message.ID, "result": nil})
			return
		}
	}
}

func testRange(startLine, startCharacter, endLine, endCharacter int) map[string]any {
	return map[string]any{
		"start": map[string]int{"line": startLine, "character": startCharacter},
		"end":   map[string]int{"line": endLine, "character": endCharacter},
	}
}

func readHelperMessage(reader *bufio.Reader) (rpcMessage, error) {
	client := client{stdout: reader}
	return client.read()
}

func writeHelperMessage(message any) {
	payload, _ := json.Marshal(message)
	fmt.Fprintf(os.Stdout, "Content-Length: %d\r\n\r\n%s", len(payload), payload)
}
