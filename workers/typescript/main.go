package main

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

const maxMessageBytes = 16 << 20

type position struct {
	Line      int `json:"line"`
	Character int `json:"character"`
}

type location struct {
	URI   string   `json:"uri"`
	Range lspRange `json:"range"`
}

type rpcError struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
}

type rpcMessage struct {
	ID     json.RawMessage `json:"id"`
	Method string          `json:"method"`
	Params json.RawMessage `json:"params"`
	Result json.RawMessage `json:"result"`
	Error  *rpcError       `json:"error"`
}

type reply struct {
	message rpcMessage
	err     error
}

type client struct {
	command *exec.Cmd
	stdin   io.WriteCloser
	stdout  *bufio.Reader
	stderr  *bytes.Buffer
	replies chan reply
	nextID  int
	opened  map[string]bool
	writeMu sync.Mutex
}

func main() {
	socket := flag.String("socket", "", "worker gRPC Unix socket")
	cacheDir := flag.String("cache-dir", "", "worker cache directory")
	root := flag.String("root", ".", "TypeScript repository root")
	file := flag.String("file", "", "repository-relative TypeScript source path")
	line := flag.Int("line", 0, "one-based source line")
	column := flag.Int("column", 0, "one-based source column")
	timeout := flag.Duration("timeout", 15*time.Second, "compiler startup and request timeout")
	flag.Parse()

	if *socket != "" {
		if err := serve(*socket, *cacheDir); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	if err := run(*root, *file, *line, *column, *timeout); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func run(root, file string, line, column int, timeout time.Duration) error {
	if file == "" || line < 1 || column < 1 {
		return errors.New("file, line, and column are required; positions are one-based")
	}

	root, err := filepath.Abs(root)
	if err != nil {
		return fmt.Errorf("resolve repository root: %w", err)
	}
	source, err := repositoryPath(root, file)
	if err != nil {
		return err
	}
	executable, err := typescriptExecutable(root)
	if err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	version, err := typescriptVersion(ctx, executable, root)
	if err != nil {
		return err
	}

	c, err := startClient(ctx, executable, root, "--lsp", "--stdio")
	if err != nil {
		return err
	}
	defer c.close()

	if err := c.initialize(ctx, root); err != nil {
		return err
	}
	definitions, err := c.definitions(ctx, source, position{Line: line - 1, Character: column - 1})
	if err != nil {
		return err
	}
	if len(definitions) != 1 {
		return fmt.Errorf("definition is not deterministic: got %d locations", len(definitions))
	}
	return json.NewEncoder(os.Stdout).Encode(struct {
		CompilerVersion string   `json:"compilerVersion"`
		Definition      location `json:"definition"`
	}{version, definitions[0]})
}

func repositoryPath(root, path string) (string, error) {
	if !filepath.IsAbs(path) {
		path = filepath.Join(root, path)
	}
	path, err := filepath.Abs(path)
	if err != nil {
		return "", fmt.Errorf("resolve source path: %w", err)
	}
	relative, err := filepath.Rel(root, path)
	if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("source path %q is outside repository %q", path, root)
	}
	return path, nil
}

func typescriptExecutable(root string) (string, error) {
	for _, name := range []string{"tsgo", "tsc"} {
		executable := filepath.Join(root, "node_modules", ".bin", name)
		info, err := os.Stat(executable)
		if err != nil {
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			return "", fmt.Errorf("repository TypeScript compiler %q: %w", executable, err)
		}
		if info.Mode().IsRegular() && info.Mode().Perm()&0o111 != 0 {
			return executable, nil
		}
	}
	return "", fmt.Errorf("repository TypeScript compiler is unavailable under %q", filepath.Join(root, "node_modules", ".bin"))
}

func typescriptVersion(ctx context.Context, executable, root string) (string, error) {
	command := exec.CommandContext(ctx, executable, "--version")
	command.Dir = root
	output, err := command.Output()
	if err != nil {
		return "", fmt.Errorf("read TypeScript compiler version: %w", err)
	}
	version := strings.TrimSpace(string(output))
	if !strings.HasPrefix(version, "Version 7.") {
		return "", fmt.Errorf("repository compiler is not TypeScript 7: %s", version)
	}
	return strings.TrimPrefix(version, "Version "), nil
}

func startClient(ctx context.Context, executable, root string, arguments ...string) (*client, error) {
	command := exec.CommandContext(ctx, executable, arguments...)
	command.Dir = root
	if os.Getenv("GOMEMLIMIT") == "" {
		command.Env = append(os.Environ(), "GOMEMLIMIT=4GiB")
	}
	stderr := &bytes.Buffer{}
	command.Stderr = stderr
	stdin, err := command.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("open compiler stdin: %w", err)
	}
	stdout, err := command.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("open compiler stdout: %w", err)
	}
	if err := command.Start(); err != nil {
		return nil, fmt.Errorf("start TypeScript language server: %w", err)
	}
	c := &client{
		command: command,
		stdin:   stdin,
		stdout:  bufio.NewReader(stdout),
		stderr:  stderr,
		replies: make(chan reply, 1),
		opened:  make(map[string]bool),
	}
	go c.readLoop()
	return c, nil
}

func (c *client) initialize(ctx context.Context, root string) error {
	rootURI := fileURI(root)
	var result struct {
		ServerInfo *struct {
			Name    string `json:"name"`
			Version string `json:"version"`
		} `json:"serverInfo"`
	}
	if err := c.request(ctx, "initialize", map[string]any{
		"processId": os.Getpid(),
		"rootUri":   rootURI,
		"capabilities": map[string]any{
			"textDocument": map[string]any{"definition": map[string]any{}},
			"window":       map[string]any{"workDoneProgress": false},
		},
		"workspaceFolders": []map[string]string{{"uri": rootURI, "name": filepath.Base(root)}},
	}, &result); err != nil {
		return fmt.Errorf("initialize TypeScript language server: %w", err)
	}
	return c.notify("initialized", map[string]any{})
}

func (c *client) open(path string) error {
	if c.opened[path] {
		return nil
	}
	content, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("read source: %w", err)
	}
	return c.openContent(path, content)
}

func (c *client) openContent(path string, content []byte) error {
	if c.opened[path] {
		return nil
	}
	uri := fileURI(path)
	if err := c.notify("textDocument/didOpen", map[string]any{
		"textDocument": map[string]any{
			"uri": uri, "languageId": languageID(path), "version": 1, "text": string(content),
		},
	}); err != nil {
		return err
	}
	c.opened[path] = true
	return nil
}

func (c *client) definitions(ctx context.Context, path string, at position) ([]location, error) {
	if err := c.open(path); err != nil {
		return nil, err
	}
	uri := fileURI(path)

	var raw json.RawMessage
	if err := c.request(ctx, "textDocument/definition", map[string]any{
		"textDocument": map[string]string{"uri": uri},
		"position":     at,
	}, &raw); err != nil {
		return nil, fmt.Errorf("request definition: %w", err)
	}
	return decodeLocations(raw)
}

func (c *client) request(ctx context.Context, method string, params, result any) error {
	c.nextID++
	id := c.nextID
	if err := c.write(map[string]any{"jsonrpc": "2.0", "id": id, "method": method, "params": params}); err != nil {
		return err
	}

	for {
		var reply reply
		select {
		case <-ctx.Done():
			return ctx.Err()
		case reply = <-c.replies:
		}
		if reply.err != nil {
			return reply.err
		}
		if reply.message.Method != "" && len(reply.message.ID) != 0 {
			if err := c.write(map[string]any{"jsonrpc": "2.0", "id": reply.message.ID, "result": nil}); err != nil {
				return err
			}
			continue
		}
		if string(reply.message.ID) != strconv.Itoa(id) {
			continue
		}
		if reply.message.Error != nil {
			return fmt.Errorf("LSP error %d: %s", reply.message.Error.Code, reply.message.Error.Message)
		}
		if result == nil || bytes.Equal(reply.message.Result, []byte("null")) {
			return nil
		}
		if raw, ok := result.(*json.RawMessage); ok {
			*raw = append((*raw)[:0], reply.message.Result...)
			return nil
		}
		return json.Unmarshal(reply.message.Result, result)
	}
}

func (c *client) notify(method string, params any) error {
	message := map[string]any{"jsonrpc": "2.0", "method": method}
	if params != nil {
		message["params"] = params
	}
	return c.write(message)
}

func (c *client) write(message any) error {
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	payload, err := json.Marshal(message)
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(c.stdin, "Content-Length: %d\r\n\r\n%s", len(payload), payload)
	return err
}

func (c *client) read() (rpcMessage, error) {
	length := -1
	for {
		line, err := c.stdout.ReadString('\n')
		if err != nil {
			return rpcMessage{}, err
		}
		line = strings.TrimSpace(line)
		if line == "" {
			break
		}
		name, value, ok := strings.Cut(line, ":")
		if ok && strings.EqualFold(name, "Content-Length") {
			length, err = strconv.Atoi(strings.TrimSpace(value))
			if err != nil {
				return rpcMessage{}, fmt.Errorf("invalid LSP content length: %w", err)
			}
		}
	}
	if length < 0 || length > maxMessageBytes {
		return rpcMessage{}, fmt.Errorf("invalid LSP message size %d", length)
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(c.stdout, payload); err != nil {
		return rpcMessage{}, err
	}
	var message rpcMessage
	if err := json.Unmarshal(payload, &message); err != nil {
		return rpcMessage{}, fmt.Errorf("decode LSP message: %w", err)
	}
	return message, nil
}

func (c *client) readLoop() {
	for {
		message, err := c.read()
		if err != nil {
			c.replies <- reply{err: err}
			return
		}
		if message.Method != "" {
			if len(message.ID) != 0 {
				if err := c.write(map[string]any{"jsonrpc": "2.0", "id": message.ID, "result": nil}); err != nil {
					c.replies <- reply{err: err}
					return
				}
			}
			continue
		}
		c.replies <- reply{message: message}
	}
}

func (c *client) close() {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	_ = c.request(ctx, "shutdown", nil, nil)
	_ = c.notify("exit", nil)
	done := make(chan error, 1)
	go func() { done <- c.command.Wait() }()
	var err error
	select {
	case err = <-done:
	case <-ctx.Done():
		_ = c.command.Process.Kill()
		err = <-done
	}
	_ = c.stdin.Close()
	if err != nil && !strings.Contains(c.stderr.String(), "context canceled") {
		fmt.Fprintf(os.Stderr, "TypeScript language server exited: %v: %s\n", err, strings.TrimSpace(c.stderr.String()))
	}
}

func decodeLocations(raw json.RawMessage) ([]location, error) {
	if len(raw) == 0 || bytes.Equal(raw, []byte("null")) {
		return nil, nil
	}
	if raw[0] == '{' {
		raw = append(append([]byte{'['}, raw...), ']')
	}
	var values []struct {
		location
		TargetURI            string    `json:"targetUri"`
		TargetSelectionRange *lspRange `json:"targetSelectionRange"`
	}
	if err := json.Unmarshal(raw, &values); err != nil {
		return nil, fmt.Errorf("decode definition locations: %w", err)
	}
	locations := make([]location, 0, len(values))
	for _, value := range values {
		if value.TargetURI != "" && value.TargetSelectionRange != nil {
			value.URI = value.TargetURI
			value.Range = *value.TargetSelectionRange
		}
		if value.URI == "" {
			return nil, errors.New("definition response omitted URI")
		}
		locations = append(locations, value.location)
	}
	return locations, nil
}

func fileURI(path string) string {
	return (&url.URL{Scheme: "file", Path: filepath.ToSlash(path)}).String()
}

func languageID(path string) string {
	switch filepath.Ext(path) {
	case ".tsx":
		return "typescriptreact"
	case ".js":
		return "javascript"
	case ".jsx":
		return "javascriptreact"
	default:
		return "typescript"
	}
}
