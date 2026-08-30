package main

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net"
	"net/url"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"time"
	"unicode/utf16"

	beholderv1 "github.com/benediktms/beholder/workers/typescript/internal/proto/beholder/v1"
	workerv1 "github.com/benediktms/beholder/workers/typescript/internal/proto/beholder/worker/v1"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/metric"
	"go.opentelemetry.io/otel/trace"
	"google.golang.org/grpc"
)

const (
	analyzerVersion = "1:typescript-compiler:3"
	// ponytail: bound exhaustive LSP fan-out; replace with persisted batching when partial enrichment can merge.
	maxCompilerCandidates = 500
	maxAnalysisDuration   = 5 * time.Minute
)

type analyzerServer struct {
	workerv1.UnimplementedAnalyzerWorkerServer
	telemetry workerTelemetry
}

type workerTelemetry struct {
	tracer              trace.Tracer
	compilerInvocations metric.Int64Counter
	candidates          metric.Int64Counter
	overrides           metric.Int64Counter
	diagnostics         metric.Int64Counter
	requests            metric.Int64Counter
	duration            metric.Float64Histogram
	processMemory       metric.Int64Histogram
}

type repositorySnapshot struct {
	identity string
	base     string
	target   bool
	inputs   map[string][]byte
}

type analysisSnapshot struct {
	workspace    string
	repositories map[string]*repositorySnapshot
	entities     map[string]bool
	candidates   []*workerv1.SemanticCandidate
}

type analysisResult struct {
	compilerVersion string
	overrides       []*workerv1.CandidateOverride
	diagnostics     []*workerv1.AnalysisDiagnostic
	failureCode     string
	failureMessage  string
}

type documentSymbol struct {
	Name           string           `json:"name"`
	ContainerName  string           `json:"containerName"`
	Range          lspRange         `json:"range"`
	SelectionRange lspRange         `json:"selectionRange"`
	Location       *location        `json:"location"`
	Children       []documentSymbol `json:"children"`
}

type lspRange struct {
	Start position `json:"start"`
	End   position `json:"end"`
}

type compilerRequestError struct{ error }

func serve(socket, _ string) error {
	if err := os.MkdirAll(filepath.Dir(socket), 0o700); err != nil {
		return fmt.Errorf("create socket directory: %w", err)
	}
	listener, err := net.Listen("unix", socket)
	if err != nil {
		return fmt.Errorf("listen on worker socket: %w", err)
	}
	defer listener.Close()
	server := grpc.NewServer(
		grpc.MaxRecvMsgSize(64<<20),
		grpc.MaxSendMsgSize(64<<20),
	)
	workerv1.RegisterAnalyzerWorkerServer(server, &analyzerServer{telemetry: newWorkerTelemetry()})
	slog.Info("TypeScript enrichment worker started", "socket", socket)
	return server.Serve(listener)
}

func (s *analyzerServer) Analyze(stream grpc.BidiStreamingServer[workerv1.AnalyzeRequest, workerv1.AnalyzeEvent]) error {
	started := time.Now()
	if err := stream.Send(progressEvent(workerv1.AnalysisPhase_ANALYSIS_PHASE_RECEIVING_SNAPSHOT, "receiving immutable baseline")); err != nil {
		return err
	}
	snapshot, err := receiveSnapshot(stream)
	if err != nil {
		return err
	}
	target, err := snapshot.target()
	if err != nil {
		return err
	}
	if err := stream.Send(progressEvent(workerv1.AnalysisPhase_ANALYSIS_PHASE_ANALYZING, "querying repository TypeScript compiler")); err != nil {
		return err
	}
	ctx, span := s.telemetry.tracer.Start(stream.Context(), "typescript.compiler.enrichment",
		trace.WithAttributes(
			attribute.String("workspace", snapshot.workspace),
			attribute.String("repository", target.identity),
			attribute.Int("candidate.count", len(snapshot.candidates)),
		),
	)
	result := analyzeSnapshot(ctx, snapshot, target, func(completed, total int) error {
		return stream.Send(progressEvent(
			workerv1.AnalysisPhase_ANALYSIS_PHASE_ANALYZING,
			fmt.Sprintf("queried %d/%d compiler candidates", completed, total),
		))
	})
	elapsed := time.Since(started).Seconds() * 1_000
	var memory runtime.MemStats
	runtime.ReadMemStats(&memory)
	outcome := "complete"
	if result.failureCode != "" {
		outcome = "failed"
	} else if len(result.diagnostics) > 0 {
		outcome = "incomplete"
	}
	metricAttributes := metric.WithAttributes(
		attribute.String("repository", target.identity),
		attribute.String("compiler.version", result.compilerVersion),
		attribute.String("outcome", outcome),
	)
	if len(snapshot.candidates) > 0 {
		s.telemetry.compilerInvocations.Add(ctx, 1, metricAttributes)
	}
	s.telemetry.candidates.Add(ctx, int64(len(snapshot.candidates)), metricAttributes)
	s.telemetry.overrides.Add(ctx, int64(len(result.overrides)), metricAttributes)
	s.telemetry.diagnostics.Add(ctx, int64(len(result.diagnostics)), metricAttributes)
	s.telemetry.requests.Add(ctx, 1, metricAttributes)
	s.telemetry.duration.Record(ctx, elapsed, metricAttributes)
	s.telemetry.processMemory.Record(ctx, int64(memory.Sys), metricAttributes)
	span.SetAttributes(
		attribute.String("compiler.version", result.compilerVersion),
		attribute.Int("override.count", len(result.overrides)),
		attribute.Int("diagnostic.count", len(result.diagnostics)),
	)
	span.End()
	if result.failureCode != "" {
		slog.Error("TypeScript compiler enrichment failed",
			"workspace", snapshot.workspace,
			"repository", target.identity,
			"candidate.count", len(snapshot.candidates),
			"code", result.failureCode,
			"error", result.failureMessage,
			"elapsed_ms", int64(elapsed),
			"process_memory_bytes", memory.Sys,
		)
		return stream.Send(&workerv1.AnalyzeEvent{Event: &workerv1.AnalyzeEvent_Failure{Failure: &workerv1.AnalysisFailure{
			Code: result.failureCode, Message: result.failureMessage,
		}}})
	}
	completeness := workerv1.AnalysisCompleteness_ANALYSIS_COMPLETENESS_COMPLETE
	if len(result.diagnostics) > 0 {
		completeness = workerv1.AnalysisCompleteness_ANALYSIS_COMPLETENESS_INCOMPLETE
	}
	if err := stream.Send(&workerv1.AnalyzeEvent{Event: &workerv1.AnalyzeEvent_Repository{Repository: &workerv1.RepositoryContribution{
		Repository:              target.identity,
		Completeness:            completeness,
		Diagnostics:             result.diagnostics,
		ReplacedDiagnosticCodes: []string{"typescript.receiver_resolution_incomplete"},
	}}}); err != nil {
		return err
	}
	if len(result.overrides) > 0 {
		if err := stream.Send(&workerv1.AnalyzeEvent{Event: &workerv1.AnalyzeEvent_Contribution{Contribution: &workerv1.AnalysisContribution{
			CandidateOverrides: result.overrides,
		}}}); err != nil {
			return err
		}
	}
	slog.Info("TypeScript compiler enrichment completed",
		"workspace", snapshot.workspace,
		"repository", target.identity,
		"compiler.version", result.compilerVersion,
		"candidate.count", len(snapshot.candidates),
		"override.count", len(result.overrides),
		"diagnostic.count", len(result.diagnostics),
		"elapsed_ms", int64(elapsed),
		"process_memory_bytes", memory.Sys,
	)
	return stream.Send(&workerv1.AnalyzeEvent{Event: &workerv1.AnalyzeEvent_Completed{Completed: &workerv1.AnalysisCompleted{
		Metadata:           &workerv1.AnalyzerMetadata{Id: "typescript", Version: analyzerVersion},
		ActiveRepositories: []string{target.identity},
		Cache:              &workerv1.CacheStatistics{Misses: 1},
	}}})
}

func newWorkerTelemetry() workerTelemetry {
	meter := otel.Meter("beholder.worker.typescript")
	compilerInvocations, _ := meter.Int64Counter("beholder.typescript.compiler.invocations")
	candidates, _ := meter.Int64Counter("beholder.typescript.candidates")
	overrides, _ := meter.Int64Counter("beholder.typescript.overrides")
	diagnostics, _ := meter.Int64Counter("beholder.typescript.diagnostics")
	requests, _ := meter.Int64Counter("beholder.typescript.requests")
	duration, _ := meter.Float64Histogram("beholder.typescript.enrichment.duration", metric.WithUnit("ms"))
	processMemory, _ := meter.Int64Histogram("beholder.typescript.process.memory", metric.WithUnit("By"))
	return workerTelemetry{
		tracer:              otel.Tracer("beholder.worker.typescript"),
		compilerInvocations: compilerInvocations,
		candidates:          candidates,
		overrides:           overrides,
		diagnostics:         diagnostics,
		requests:            requests,
		duration:            duration,
		processMemory:       processMemory,
	}
}

func receiveSnapshot(stream grpc.BidiStreamingServer[workerv1.AnalyzeRequest, workerv1.AnalyzeEvent]) (*analysisSnapshot, error) {
	snapshot := &analysisSnapshot{repositories: make(map[string]*repositorySnapshot), entities: make(map[string]bool)}
	for {
		request, err := stream.Recv()
		if err != nil {
			return nil, err
		}
		switch value := request.Request.(type) {
		case *workerv1.AnalyzeRequest_Start:
			if snapshot.workspace != "" || value.Start.GetWorkspace() == "" {
				return nil, errors.New("analysis start is missing or repeated")
			}
			snapshot.workspace = value.Start.GetWorkspace()
		case *workerv1.AnalyzeRequest_Repository:
			repository := value.Repository
			if repository.GetIdentity() == "" || repository.GetBase() == "" || snapshot.repositories[repository.GetIdentity()] != nil {
				return nil, errors.New("repository snapshot is invalid or repeated")
			}
			snapshot.repositories[repository.GetIdentity()] = &repositorySnapshot{
				identity: repository.GetIdentity(), base: repository.GetBase(), target: repository.GetTarget(), inputs: make(map[string][]byte),
			}
		case *workerv1.AnalyzeRequest_Input:
			input := value.Input
			repository := snapshot.repositories[input.GetRepository()]
			if repository == nil || input.GetPath() == "" || repository.inputs[input.GetPath()] != nil {
				return nil, errors.New("repository input is invalid or repeated")
			}
			repository.inputs[input.GetPath()] = bytes.Clone(input.GetContent())
		case *workerv1.AnalyzeRequest_BaselineEntity:
			entity := value.BaselineEntity.GetEntity()
			if entity == nil || entity.GetId() == "" || snapshot.entities[entity.GetId()] {
				return nil, errors.New("baseline entity is invalid or repeated")
			}
			snapshot.entities[entity.GetId()] = true
		case *workerv1.AnalyzeRequest_BaselineCandidate:
			candidate := value.BaselineCandidate.GetCandidate()
			if candidate == nil || candidate.GetId() == "" || candidate.GetSpan() == nil {
				return nil, errors.New("baseline semantic candidate is invalid")
			}
			snapshot.candidates = append(snapshot.candidates, candidate)
		case *workerv1.AnalyzeRequest_BaselineObservation:
		case *workerv1.AnalyzeRequest_Finish:
			if snapshot.workspace == "" {
				return nil, errors.New("analysis start is missing")
			}
			return snapshot, nil
		default:
			return nil, errors.New("analysis request is missing")
		}
	}
}

func (snapshot *analysisSnapshot) target() (*repositorySnapshot, error) {
	var target *repositorySnapshot
	for _, repository := range snapshot.repositories {
		if !repository.target {
			continue
		}
		if target != nil {
			return nil, errors.New("analysis snapshot has multiple target repositories")
		}
		target = repository
	}
	if target == nil {
		return nil, errors.New("analysis snapshot has no target repository")
	}
	return target, nil
}

func analyzeSnapshot(parent context.Context, snapshot *analysisSnapshot, target *repositorySnapshot, progress func(int, int) error) analysisResult {
	if err := verifySnapshot(snapshot); err != nil {
		return failedAnalysis("typescript.compiler.snapshot_changed", err)
	}
	if len(snapshot.candidates) == 0 {
		return analysisResult{}
	}
	parent, cancelAnalysis := context.WithTimeout(parent, maxAnalysisDuration)
	defer cancelAnalysis()
	executable, err := typescriptExecutable(target.base)
	if err != nil {
		return failedAnalysis("typescript.compiler.unavailable", err)
	}
	ctx, cancel := context.WithTimeout(parent, workerTimeout())
	version, err := typescriptVersion(ctx, executable, target.base)
	if err != nil {
		if ctx.Err() != nil {
			failure := failedCompilerRequest(ctx, err)
			cancel()
			return failure
		}
		cancel()
		return failedAnalysis("typescript.compiler.unavailable", err)
	}
	cancel()
	processCtx, stop := context.WithCancel(parent)
	defer stop()
	c, err := startClient(processCtx, executable, target.base, "--lsp", "--stdio")
	if err != nil {
		return failedAnalysis("typescript.compiler.unavailable", err)
	}
	defer c.close()
	ctx, cancel = context.WithTimeout(parent, workerTimeout())
	err = c.initialize(ctx, target.base)
	if err != nil {
		failure := failedCompilerRequest(ctx, err)
		cancel()
		return failure
	}
	cancel()
	result := analysisResult{compilerVersion: version}
	candidates, skipped := boundedCandidates(snapshot.candidates)
	if skipped > 0 {
		result.diagnostics = append(result.diagnostics, diagnostic(
			"typescript.compiler.candidate_limit",
			"",
			0,
			fmt.Errorf("deferred %d compiler candidates after the deterministic %d-candidate limit", skipped, maxCompilerCandidates),
		))
	}
	seen := make(map[string]bool)
	for index, candidate := range candidates {
		if progress != nil && index > 0 && index%100 == 0 {
			if err := progress(index, len(candidates)); err != nil {
				return failedAnalysis("typescript.compiler.cancelled", err)
			}
		}
		if candidate.GetRepository() != target.identity || seen[candidate.GetId()] {
			result.diagnostics = append(result.diagnostics, candidateDiagnostic("typescript.compiler.candidate_invalid", candidate, errors.New("candidate ownership or ID is invalid")))
			continue
		}
		seen[candidate.GetId()] = true
		path, err := repositoryPath(target.base, candidate.GetSpan().GetPath())
		if err != nil {
			result.diagnostics = append(result.diagnostics, candidateDiagnostic("typescript.compiler.candidate_invalid", candidate, err))
			continue
		}
		ctx, cancel = context.WithTimeout(parent, workerTimeout())
		definitions, err := c.definitions(ctx, path, position{Line: int(candidate.GetSpan().GetStart().GetLine()), Character: int(candidate.GetSpan().GetStart().GetCharacter())})
		if err != nil {
			failure := failedCompilerRequest(ctx, err)
			cancel()
			return failure
		}
		if len(definitions) != 1 {
			cancel()
			code := "typescript.compiler.definition_missing"
			if len(definitions) > 1 {
				code = "typescript.compiler.definition_ambiguous"
			}
			result.diagnostics = append(result.diagnostics, candidateDiagnostic(code, candidate, fmt.Errorf("compiler returned %d definitions", len(definitions))))
			continue
		}
		resolved, evidence, err := mapDefinition(ctx, c, definitions[0], snapshot)
		if err != nil {
			var requestError compilerRequestError
			if errors.As(err, &requestError) {
				failure := failedCompilerRequest(ctx, requestError.error)
				cancel()
				return failure
			}
			cancel()
			result.diagnostics = append(result.diagnostics, candidateDiagnostic("typescript.compiler.definition_unmapped", candidate, err))
			continue
		}
		cancel()
		result.overrides = append(result.overrides, &workerv1.CandidateOverride{CandidateId: candidate.GetId(), ResolvedTo: resolved, Evidence: fmt.Sprintf("TypeScript %s definition %s", version, evidence)})
	}
	if progress != nil {
		if err := progress(len(candidates), len(candidates)); err != nil {
			return failedAnalysis("typescript.compiler.cancelled", err)
		}
	}
	if err := verifySnapshot(snapshot); err != nil {
		return failedAnalysis("typescript.compiler.snapshot_changed", err)
	}
	return result
}

func boundedCandidates(input []*workerv1.SemanticCandidate) ([]*workerv1.SemanticCandidate, int) {
	candidates := append([]*workerv1.SemanticCandidate(nil), input...)
	sort.Slice(candidates, func(i, j int) bool {
		leftStorybook, rightStorybook := isStorybookCandidate(candidates[i]), isStorybookCandidate(candidates[j])
		if leftStorybook != rightStorybook {
			return !leftStorybook
		}
		left, right := candidatePriority(candidates[i]), candidatePriority(candidates[j])
		if left != right {
			return left < right
		}
		return candidates[i].GetId() < candidates[j].GetId()
	})
	if len(candidates) <= maxCompilerCandidates {
		return candidates, 0
	}
	return candidates[:maxCompilerCandidates], len(candidates) - maxCompilerCandidates
}

func candidatePriority(candidate *workerv1.SemanticCandidate) int {
	target := candidate.GetUnresolvedTo()
	switch {
	case strings.HasPrefix(target, "javascript-call://"):
		return 0
	case strings.HasPrefix(target, "typescript-call://"):
		return 1
	case strings.HasPrefix(target, "typescript-method://"):
		return 2
	default:
		return 3
	}
}

func failedCompilerRequest(ctx context.Context, err error) analysisResult {
	if errors.Is(ctx.Err(), context.DeadlineExceeded) {
		return failedAnalysis("typescript.compiler.timeout", err)
	}
	if errors.Is(ctx.Err(), context.Canceled) {
		return failedAnalysis("typescript.compiler.cancelled", err)
	}
	return failedAnalysis("typescript.compiler.request_failed", err)
}

func failedAnalysis(code string, err error) analysisResult {
	return analysisResult{failureCode: code, failureMessage: err.Error()}
}

func verifySnapshot(snapshot *analysisSnapshot) error {
	for _, repository := range snapshot.repositories {
		for path, expected := range repository.inputs {
			resolved, err := repositoryPath(repository.base, path)
			if err != nil {
				return err
			}
			actual, err := os.ReadFile(resolved)
			if err != nil || !bytes.Equal(actual, expected) {
				return fmt.Errorf("repository input changed: %s/%s", repository.identity, path)
			}
		}
	}
	return nil
}

func mapDefinition(ctx context.Context, c *client, definition location, snapshot *analysisSnapshot) (string, string, error) {
	path, err := pathFromFileURI(definition.URI)
	if err != nil {
		return "", "", err
	}
	repository, relative := owningRepository(path, snapshot.repositories)
	if repository == nil {
		return "", "", fmt.Errorf("definition is outside the immutable workspace: %s", path)
	}
	module, err := moduleID(repository.identity, relative)
	if err != nil {
		return "", "", err
	}
	symbols, err := c.documentSymbols(ctx, path)
	if err != nil {
		return "", "", compilerRequestError{err}
	}
	if names := symbolPath(symbols, definition.Range.Start, nil); len(names) > 0 {
		id := module + "/" + strings.Join(names, "/")
		if snapshot.entities[id] {
			return id, fmt.Sprintf("%s:%d:%d", relative, definition.Range.Start.Line+1, definition.Range.Start.Character+1), nil
		}
	}
	content, err := os.ReadFile(path)
	if err != nil {
		return "", "", err
	}
	name, err := selectedText(content, definition.Range)
	if err != nil {
		return "", "", err
	}
	var matches []string
	for id := range snapshot.entities {
		if strings.HasPrefix(id, module+"/") && strings.HasSuffix(id, "/"+name) {
			matches = append(matches, id)
		}
	}
	sort.Strings(matches)
	if len(matches) != 1 {
		return "", "", fmt.Errorf("definition %s has %d canonical entity matches", name, len(matches))
	}
	return matches[0], fmt.Sprintf("%s:%d:%d", relative, definition.Range.Start.Line+1, definition.Range.Start.Character+1), nil
}

func isTypeScriptSource(path string) bool {
	switch filepath.Ext(path) {
	case ".ts", ".tsx", ".js", ".jsx":
		return true
	default:
		return false
	}
}

func (c *client) documentSymbols(ctx context.Context, path string) ([]documentSymbol, error) {
	if err := c.open(path); err != nil {
		return nil, err
	}
	var symbols []documentSymbol
	if err := c.request(ctx, "textDocument/documentSymbol", map[string]any{"textDocument": map[string]string{"uri": fileURI(path)}}, &symbols); err != nil {
		return nil, err
	}
	return symbols, nil
}

func symbolPath(symbols []documentSymbol, at position, parent []string) []string {
	for _, symbol := range symbols {
		range_ := symbol.SelectionRange
		if symbol.Location != nil {
			range_ = symbol.Location.Range
		}
		path := append(append([]string(nil), parent...), symbol.Name)
		if child := symbolPath(symbol.Children, at, path); len(child) > 0 {
			return child
		}
		if range_.Start == at {
			if symbol.ContainerName != "" {
				return append(strings.Split(symbol.ContainerName, "."), symbol.Name)
			}
			return path
		}
	}
	return nil
}

func owningRepository(path string, repositories map[string]*repositorySnapshot) (*repositorySnapshot, string) {
	var selected *repositorySnapshot
	var selectedRelative string
	for _, repository := range repositories {
		relative, err := filepath.Rel(repository.base, path)
		if err != nil || relative == ".." || strings.HasPrefix(relative, ".."+string(filepath.Separator)) {
			continue
		}
		if selected == nil || len(repository.base) > len(selected.base) {
			selected, selectedRelative = repository, filepath.ToSlash(relative)
		}
	}
	return selected, selectedRelative
}

func moduleID(repository, relative string) (string, error) {
	extension := filepath.Ext(relative)
	language := "typescript"
	if extension == ".js" || extension == ".jsx" {
		language = "javascript"
	} else if extension != ".ts" && extension != ".tsx" {
		return "", fmt.Errorf("definition is not a TypeScript source: %s", relative)
	}
	stem := strings.TrimSuffix(filepath.ToSlash(relative), extension)
	return fmt.Sprintf("repo://%s/%s/%s", repository, language, stem), nil
}

func pathFromFileURI(value string) (string, error) {
	parsed, err := url.Parse(value)
	if err != nil || parsed.Scheme != "file" || (parsed.Host != "" && parsed.Host != "localhost") {
		return "", fmt.Errorf("definition URI is not a local file: %s", value)
	}
	path, err := url.PathUnescape(parsed.Path)
	if err != nil {
		return "", err
	}
	return filepath.FromSlash(path), nil
}

func selectedText(content []byte, range_ lspRange) (string, error) {
	if range_.Start.Line != range_.End.Line {
		return "", errors.New("definition selection spans multiple lines")
	}
	lines := bytes.Split(content, []byte("\n"))
	if range_.Start.Line < 0 || range_.Start.Line >= len(lines) {
		return "", errors.New("definition line is outside the source")
	}
	start, err := utf16ByteOffset(lines[range_.Start.Line], range_.Start.Character)
	if err != nil {
		return "", err
	}
	end, err := utf16ByteOffset(lines[range_.End.Line], range_.End.Character)
	if err != nil || end < start {
		return "", errors.New("definition character range is invalid")
	}
	return string(lines[range_.Start.Line][start:end]), nil
}

func utf16ByteOffset(line []byte, units int) (int, error) {
	count := 0
	for index, value := range string(line) {
		if count == units {
			return index, nil
		}
		count += utf16.RuneLen(value)
		if count > units {
			return 0, errors.New("UTF-16 position splits a surrogate pair")
		}
	}
	if count == units {
		return len(line), nil
	}
	return 0, errors.New("UTF-16 position is outside the source line")
}

func workerTimeout() time.Duration {
	value, err := time.ParseDuration(os.Getenv("BEHOLDER_WORKER_TIMEOUT_MS") + "ms")
	if err == nil && value > 0 {
		return value
	}
	return 10 * time.Minute
}

func diagnostic(code, path string, line uint32, err error) *workerv1.AnalysisDiagnostic {
	detail := err.Error()
	value := &workerv1.AnalysisDiagnostic{Code: code, Severity: beholderv1.AnalysisDiagnosticSeverity_ANALYSIS_DIAGNOSTIC_SEVERITY_WARNING, Path: path, Detail: &detail}
	if line > 0 {
		value.Line = &line
	}
	return value
}

func candidateDiagnostic(code string, candidate *workerv1.SemanticCandidate, err error) *workerv1.AnalysisDiagnostic {
	line := candidate.GetSpan().GetStart().GetLine() + 1
	return diagnostic(code, candidate.GetSpan().GetPath(), line, err)
}

func progressEvent(phase workerv1.AnalysisPhase, detail string) *workerv1.AnalyzeEvent {
	return &workerv1.AnalyzeEvent{Event: &workerv1.AnalyzeEvent_Progress{Progress: &workerv1.AnalysisProgress{Phase: phase, Detail: &detail}}}
}
