package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
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
	definition, err := c.definition(ctx, source, position{Line: 0, Character: 1})
	if err != nil {
		t.Fatal(err)
	}
	if definition.URI != "file:///repo/src/target.ts" || definition.Range.Start.Line != 2 {
		t.Fatalf("unexpected definition: %+v", definition)
	}
}

func runLSPHelper() {
	reader := bufio.NewReader(os.Stdin)
	for {
		message, err := readHelperMessage(reader)
		if err != nil {
			return
		}
		switch message.Method {
		case "initialize":
			writeHelperMessage(map[string]any{
				"jsonrpc": "2.0", "id": message.ID,
				"result": map[string]any{"capabilities": map[string]any{}, "serverInfo": map[string]string{"name": "typescript-go", "version": "7.0.2"}},
			})
		case "textDocument/definition":
			writeHelperMessage(map[string]any{
				"jsonrpc": "2.0", "id": message.ID,
				"result": []map[string]any{{
					"targetUri": "file:///repo/src/target.ts",
					"targetSelectionRange": map[string]any{
						"start": map[string]int{"line": 2, "character": 4},
						"end":   map[string]int{"line": 2, "character": 10},
					},
				}},
			})
		case "shutdown":
			writeHelperMessage(map[string]any{"jsonrpc": "2.0", "id": message.ID, "result": nil})
			return
		}
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
