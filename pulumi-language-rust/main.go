// Copyright 2026, Pulumi Corporation.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// pulumi-language-rust is the Pulumi language host for Rust. It serves the
// LanguageRuntime gRPC interface: running Rust Pulumi programs with cargo,
// generating Rust SDKs from Pulumi schemas, and generating Rust projects
// from PCL programs.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/BurntSushi/toml"
	pbempty "google.golang.org/protobuf/types/known/emptypb"

	"google.golang.org/grpc"
	"google.golang.org/grpc/health"
	healthgrpc "google.golang.org/grpc/health/grpc_health_v1"
	"google.golang.org/grpc/reflection"

	"github.com/pulumi/pulumi/pkg/v3/codegen/hcl2/syntax"
	"github.com/pulumi/pulumi/pkg/v3/codegen/pcl"
	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
	"github.com/pulumi/pulumi/sdk/v3/go/common/resource/plugin"
	"github.com/pulumi/pulumi/sdk/v3/go/common/util/cmdutil"
	"github.com/pulumi/pulumi/sdk/v3/go/common/util/executable"
	"github.com/pulumi/pulumi/sdk/v3/go/common/util/logging"
	"github.com/pulumi/pulumi/sdk/v3/go/common/util/rpcutil"
	"github.com/pulumi/pulumi/sdk/v3/go/common/workspace"
	pulumirpc "github.com/pulumi/pulumi/sdk/v3/proto/go"

	"github.com/lukebward/pulumi-rust/pulumi-language-rust/codegen"
)

// Version is the language host version.
var Version = "0.1.0"

// exitStatusLoggedError is the exit code a Rust SDK program uses to signal
// "the error was already logged to the engine".
const exitStatusLoggedError = 32

func main() {
	var tracing string
	flag.StringVar(&tracing, "tracing", "", "Emit tracing to a Zipkin-compatible tracing endpoint")
	flag.Parse()
	args := flag.Args()
	logging.InitLogging(false, 0, false)
	cmdutil.InitTracing("pulumi-language-rust", "pulumi-language-rust", tracing)

	var engineAddress string
	if len(args) > 0 {
		engineAddress = args[0]
	}

	ctx, cancel := context.WithCancel(context.Background())
	if engineAddress != "" {
		if err := rpcutil.Healthcheck(ctx, engineAddress, 5*time.Minute, cancel); err != nil {
			cmdutil.Exit(fmt.Errorf("could not start health check host RPC server: %w", err))
		}
	}

	cancelChannel := make(chan bool)
	go func() {
		<-ctx.Done()
		close(cancelChannel)
	}()

	handle, err := serveLanguageHost(cancelChannel, engineAddress)
	if err != nil {
		cmdutil.Exit(fmt.Errorf("could not start language host RPC server: %w", err))
	}

	fmt.Printf("%d\n", handle.Port)

	if err := <-handle.Done; err != nil {
		cmdutil.Exit(fmt.Errorf("language host RPC stopped serving: %w", err))
	}
}

// serveLanguageHost is rpcutil.ServeWithOptions without the 400 MiB cap on
// incoming gRPC messages. The cap cannot be lifted through that helper: it
// appends its own grpc.MaxRecvMsgSize after the caller's options, and the
// last option applied wins. 400 MiB is too small for GeneratePackage, whose
// request carries the provider schema inline — azure-native's is ~480 MiB.
// The cap also buys nothing here: the host listens on 127.0.0.1 for the
// engine that spawned it, and memory follows the message actually sent, not
// the ceiling.
func serveLanguageHost(cancel <-chan bool, engineAddress string) (rpcutil.ServeHandle, error) {
	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		return rpcutil.ServeHandle{}, fmt.Errorf("failed to listen on 127.0.0.1: %w", err)
	}

	srv := grpc.NewServer(append(
		rpcutil.OpenTracingServerInterceptorOptions(nil),
		grpc.MaxRecvMsgSize(math.MaxInt32),
	)...)
	pulumirpc.RegisterLanguageRuntimeServer(srv, newLanguageHost(engineAddress))

	// The rest mirrors rpcutil: health and reflection services, a cancel
	// channel that ends the server when closed or sent true, and a done
	// channel that reports how serving ended.
	healthSrv := health.NewServer()
	healthgrpc.RegisterHealthServer(srv, healthSrv)
	reflection.Register(srv)
	for name := range srv.GetServiceInfo() {
		healthSrv.SetServingStatus(name, healthgrpc.HealthCheckResponse_SERVING)
	}

	go func() {
		for v, ok := <-cancel; !v && ok; v, ok = <-cancel {
		}
		srv.GracefulStop()
	}()

	done := make(chan error)
	go func() {
		if err := srv.Serve(lis); err != nil && !rpcutil.IsBenignCloseErr(err) {
			done <- fmt.Errorf("stopped serving: %w", err)
		} else {
			done <- nil
		}
		close(done)
	}()

	return rpcutil.ServeHandle{Port: lis.Addr().(*net.TCPAddr).Port, Done: done}, nil
}

type rustLanguageHost struct {
	pulumirpc.UnimplementedLanguageRuntimeServer

	engineAddress string
}

func newLanguageHost(engineAddress string) pulumirpc.LanguageRuntimeServer {
	return &rustLanguageHost{engineAddress: engineAddress}
}

// sharedTargetDir returns a stable cargo target directory shared by every
// build the host runs, so dependencies compile once per machine, not once
// per generated project.
//
// It lives under the user's cache directory rather than the system temp
// directory. A predictable path in a world-writable /tmp is another local
// user's to claim: they can pre-create it, or point a symlink at a directory
// they control, and cargo will then write — and later execute — build-script
// binaries from a location they can rewrite at will.
//
// Returns "" when no private cache directory can be located, in which case
// the caller leaves CARGO_TARGET_DIR unset and cargo falls back to each
// project's own `target/`. That costs a recompile per project but is never
// unsafe; sharing is a performance measure, not a correctness one.
func sharedTargetDir() string {
	cache, err := os.UserCacheDir()
	if err != nil {
		return ""
	}
	dir := filepath.Join(cache, "pulumi-language-rust", "target")
	// 0o700 so the directory is ours even if the cache root is permissive.
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return ""
	}
	return dir
}

func cargoCommand(ctx context.Context, dir string, extraEnv []string, args ...string) (*exec.Cmd, error) {
	cargo, err := executable.FindExecutable("cargo")
	if err != nil {
		return nil, fmt.Errorf("could not find cargo on PATH: %w", err)
	}
	cmd := exec.CommandContext(ctx, cargo, args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		// Debug info dominates build-artifact size; conformance runs build
		// hundreds of crates, so keep artifacts lean.
		"CARGO_PROFILE_DEV_DEBUG=false",
		// Incremental artifacts add gigabytes across a full suite run and
		// buy nothing: each generated project is built once.
		"CARGO_INCREMENTAL=0",
	)
	if target := sharedTargetDir(); target != "" {
		cmd.Env = append(cmd.Env, "CARGO_TARGET_DIR="+target)
	}
	cmd.Env = append(cmd.Env, extraEnv...)
	return cmd, nil
}

func (host *rustLanguageHost) GetPluginInfo(ctx context.Context, req *pbempty.Empty) (*pulumirpc.PluginInfo, error) {
	return &pulumirpc.PluginInfo{Version: Version}, nil
}

func (host *rustLanguageHost) About(ctx context.Context, req *pulumirpc.AboutRequest) (*pulumirpc.AboutResponse, error) {
	cargo, err := executable.FindExecutable("cargo")
	if err != nil {
		return nil, err
	}
	out, err := exec.CommandContext(ctx, cargo, "--version").Output()
	if err != nil {
		return nil, fmt.Errorf("running cargo --version: %w", err)
	}
	version := strings.TrimSpace(string(out))
	return &pulumirpc.AboutResponse{
		Executable: cargo,
		Version:    version,
	}, nil
}

func (host *rustLanguageHost) RuntimeOptionsPrompts(
	ctx context.Context, req *pulumirpc.RuntimeOptionsRequest,
) (*pulumirpc.RuntimeOptionsResponse, error) {
	return &pulumirpc.RuntimeOptionsResponse{}, nil
}

// Run executes a Rust Pulumi program with cargo run.
func (host *rustLanguageHost) Run(ctx context.Context, req *pulumirpc.RunRequest) (*pulumirpc.RunResponse, error) {
	programDir := req.GetInfo().GetProgramDirectory()

	env, err := constructRunEnv(req, host.engineAddress)
	if err != nil {
		return nil, err
	}

	cmd, err := cargoCommand(ctx, programDir, env, "run", "--quiet")
	if err != nil {
		return nil, err
	}
	var stderr bytes.Buffer
	cmd.Stdout = os.Stdout
	cmd.Stderr = io.MultiWriter(os.Stderr, &stderr)

	if err := cmd.Run(); err != nil {
		var exitErr *exec.ExitError
		if ok := asExitError(err, &exitErr); ok {
			code := exitErr.ExitCode()
			if code == exitStatusLoggedError {
				// The program already reported the error to the engine.
				return &pulumirpc.RunResponse{Error: "", Bail: true}, nil
			}
			return &pulumirpc.RunResponse{
				Error: fmt.Sprintf("Program exited with non-zero exit code: %d", code),
			}, nil
		}
		return &pulumirpc.RunResponse{Error: fmt.Sprintf("running program: %v", err)}, nil
	}
	return &pulumirpc.RunResponse{}, nil
}

// asExitError reports whether err is, or wraps, a process exit failure. A
// bare type assertion would miss anything the error has been wrapped in on
// its way up, so ask errors.As.
func asExitError(err error, target **exec.ExitError) bool {
	return errors.As(err, target)
}

func constructRunEnv(req *pulumirpc.RunRequest, engineAddress string) ([]string, error) {
	configMap := req.GetConfig()
	if configMap == nil {
		configMap = map[string]string{}
	}
	config, err := json.Marshal(configMap)
	if err != nil {
		return nil, fmt.Errorf("serializing config: %w", err)
	}
	secretKeys := req.GetConfigSecretKeys()
	if secretKeys == nil {
		secretKeys = []string{}
	}
	configSecretKeys, err := json.Marshal(secretKeys)
	if err != nil {
		return nil, fmt.Errorf("serializing config secret keys: %w", err)
	}

	var env []string
	maybeAppend := func(k, v string) {
		if v != "" {
			env = append(env, k+"="+v)
		}
	}
	maybeAppend("PULUMI_MONITOR", req.GetMonitorAddress())
	maybeAppend("PULUMI_ENGINE", engineAddress)
	maybeAppend("PULUMI_ORGANIZATION", req.GetOrganization())
	maybeAppend("PULUMI_PROJECT", req.GetProject())
	maybeAppend("PULUMI_ROOT_DIRECTORY", req.GetInfo().GetRootDirectory())
	maybeAppend("PULUMI_STACK", req.GetStack())
	maybeAppend("PULUMI_PWD", req.GetPwd())
	// Always set explicitly so an inherited PULUMI_DRY_RUN can't leak in.
	env = append(env, fmt.Sprintf("PULUMI_DRY_RUN=%v", req.GetDryRun()))
	maybeAppend("PULUMI_PARALLEL", fmt.Sprint(req.GetParallel()))
	maybeAppend("PULUMI_CONFIG", string(config))
	maybeAppend("PULUMI_CONFIG_SECRET_KEYS", string(configSecretKeys))
	return env, nil
}

// InstallDependencies builds the program so later Run calls are fast.
func (host *rustLanguageHost) InstallDependencies(
	req *pulumirpc.InstallDependenciesRequest, server pulumirpc.LanguageRuntime_InstallDependenciesServer,
) error {
	closer, stdout, stderr, err := rpcutil.MakeInstallDependenciesStreams(server, req.IsTerminal)
	if err != nil {
		return err
	}
	defer closer.Close()

	dir := req.GetInfo().GetProgramDirectory()
	cmd, err := cargoCommand(server.Context(), dir, nil, "build")
	if err != nil {
		return err
	}
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("cargo build in %s failed: %w", dir, err)
	}
	return nil
}

// Link points a plugin project's `pulumi` dependency at the packed core SDK
// artifact, so a policy pack or provider builds against the SDK the engine
// handed us rather than whatever its manifest happened to name.
func (host *rustLanguageHost) Link(
	ctx context.Context, req *pulumirpc.LinkRequest,
) (*pulumirpc.LinkResponse, error) {
	dir := req.GetInfo().GetProgramDirectory()
	if dir == "" {
		dir = req.GetInfo().GetRootDirectory()
	}
	manifest := filepath.Join(dir, "Cargo.toml")
	contents, err := os.ReadFile(manifest)
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", manifest, err)
	}

	updated := string(contents)
	for _, dep := range req.GetPackages() {
		name := dep.GetPackage().GetName()
		if name == "" || dep.GetPath() == "" {
			continue
		}
		crate := name
		if name != "pulumi" {
			crate = crateName(name)
		}
		updated = rewritePathDependency(updated, crate, dep.GetPath())
	}
	if updated != string(contents) {
		if err := os.WriteFile(manifest, []byte(updated), 0o600); err != nil {
			return nil, fmt.Errorf("writing %s: %w", manifest, err)
		}
	}
	return &pulumirpc.LinkResponse{}, nil
}

// tableHeader returns the table a `[section]` line names, or "" when the
// line is not a table header.
func tableHeader(line string) (string, bool) {
	line = strings.TrimSpace(line)
	if !strings.HasPrefix(line, "[") {
		return "", false
	}
	// Trim a trailing comment before matching the closing bracket.
	if i := strings.Index(line, "#"); i >= 0 {
		line = strings.TrimSpace(line[:i])
	}
	if !strings.HasSuffix(line, "]") {
		return "", false
	}
	return strings.TrimSpace(strings.Trim(line, "[]")), true
}

// dependencyKey reports whether a line assigns the named dependency,
// tolerating the spacing variants cargo accepts.
func dependencyKey(line, crate string) bool {
	line = strings.TrimSpace(line)
	if !strings.HasPrefix(line, crate) {
		return false
	}
	return strings.HasPrefix(strings.TrimSpace(line[len(crate):]), "=")
}

// rewritePathDependency repoints one dependency's path, adding the
// dependency when the manifest does not already name it. Only dependency
// tables are considered — `[dependencies]` and `[workspace.dependencies]`,
// the latter because a workspace shares one entry across its members — so a
// same-named dev-dependency or target-specific entry cannot shadow the real
// one.
func rewritePathDependency(manifest, crate, path string) string {
	entry := fmt.Sprintf("%s = { path = %q }", crate, path)
	isDepTable := func(name string) bool {
		return name == "dependencies" || name == "workspace.dependencies"
	}

	lines := strings.Split(manifest, "\n")
	section := ""
	rewrote := false
	depsHeader := -1
	for i := 0; i < len(lines); i++ {
		if name, ok := tableHeader(lines[i]); ok {
			section = name
			if name == "dependencies" && depsHeader < 0 {
				depsHeader = i
			}
			// The `[dependencies.<crate>]` / `[workspace.dependencies.<crate>]`
			// table form.
			if name == "dependencies."+crate || name == "workspace.dependencies."+crate {
				lines = strings.Split(replaceTable(lines, i, entry, crate), "\n")
				rewrote = true
			}
			continue
		}
		if isDepTable(section) && dependencyKey(lines[i], crate) {
			// A workspace member says `pulumi = { workspace = true }`; that
			// indirection is what the workspace entry resolves, so leave it.
			if section == "dependencies" && strings.Contains(lines[i], "workspace") {
				continue
			}
			lines = strings.Split(replaceEntry(lines, i, entry), "\n")
			rewrote = true
		}
	}
	if rewrote {
		return strings.Join(lines, "\n")
	}
	if depsHeader >= 0 {
		out := append([]string{}, lines[:depsHeader+1]...)
		out = append(out, entry)
		return strings.Join(append(out, lines[depsHeader+1:]...), "\n")
	}
	return manifest + fmt.Sprintf("\n[dependencies]\n%s\n", entry)
}

// replaceEntry swaps the assignment at i, absorbing the continuation lines
// of a multi-line inline table so no orphaned fragment is left behind.
func replaceEntry(lines []string, i int, entry string) string {
	end := i
	if strings.Count(lines[i], "{") > strings.Count(lines[i], "}") {
		for end+1 < len(lines) {
			end++
			if strings.Contains(lines[end], "}") {
				break
			}
		}
	}
	out := append([]string{}, lines[:i]...)
	out = append(out, entry)
	return strings.Join(append(out, lines[end+1:]...), "\n")
}

// replaceTable swaps a `[dependencies.<crate>]` table for an inline entry.
func replaceTable(lines []string, header int, entry, crate string) string {
	end := header
	for end+1 < len(lines) {
		if _, ok := tableHeader(lines[end+1]); ok {
			break
		}
		end++
	}
	// Leave trailing blank lines where they were rather than absorbing them
	// into the replaced table.
	for end > header && strings.TrimSpace(lines[end]) == "" {
		end--
	}
	out := append([]string{}, lines[:header]...)
	out = append(out, "[dependencies]", entry)
	return strings.Join(append(out, lines[end+1:]...), "\n")
}

// crateName mirrors the codegen's package-to-crate naming.
func crateName(pkgName string) string {
	sanitized := strings.Map(func(r rune) rune {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '_':
			return r
		case r >= 'A' && r <= 'Z':
			return r + ('a' - 'A')
		default:
			return '_'
		}
	}, pkgName)
	return "pulumi_" + sanitized
}

// RunPlugin builds and runs a Rust plugin program (a policy pack or a
// provider), streaming its output back to the engine. The engine reads the
// plugin's gRPC port from the first line of stdout, so stdout is passed
// through untouched.
func (host *rustLanguageHost) RunPlugin(
	req *pulumirpc.RunPluginRequest, server pulumirpc.LanguageRuntime_RunPluginServer,
) error {
	closer, stdout, stderr, err := rpcutil.MakeRunPluginStreams(server, false)
	if err != nil {
		return err
	}
	defer closer.Close()

	dir := req.GetInfo().GetProgramDirectory()
	if dir == "" {
		dir = req.GetPwd()
	}

	// Build first, capturing the artifact path. Running the plugin through
	// `cargo run` would make it a grandchild: killing cargo would leave the
	// plugin alive holding the output pipes, so the engine's shutdown would
	// neither stop it nor let this handler return.
	exe, err := buildPluginBinary(server.Context(), dir, req.GetEnv(), stderr)
	if err != nil {
		return err
	}

	cmd := exec.CommandContext(server.Context(), exe, req.GetArgs()...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(), req.GetEnv()...)
	cmd.Stdout = stdout
	cmd.Stderr = stderr
	// Don't wait on the output pipes forever if the plugin ignores the kill.
	cmd.WaitDelay = 5 * time.Second
	if err := cmd.Run(); err != nil {
		var exitErr *exec.ExitError
		if asExitError(err, &exitErr) {
			return server.Send(&pulumirpc.RunPluginResponse{
				Output: &pulumirpc.RunPluginResponse_Exitcode{Exitcode: int32(exitErr.ExitCode())},
			})
		}
		return fmt.Errorf("running plugin in %s: %w", dir, err)
	}
	return server.Send(&pulumirpc.RunPluginResponse{
		Output: &pulumirpc.RunPluginResponse_Exitcode{Exitcode: 0},
	})
}

// buildPluginBinary builds the plugin crate at dir and returns the path of
// the executable cargo produced. Build output goes to stderr so nothing
// precedes the port line the engine reads from the plugin's stdout.
func buildPluginBinary(
	ctx context.Context, dir string, env []string, stderr io.Writer,
) (string, error) {
	cmd, err := cargoCommand(ctx, dir, env, "build", "--message-format=json-render-diagnostics")
	if err != nil {
		return "", err
	}
	// Buffer the build's own output: cargo's progress lines would otherwise
	// reach the engine as plugin diagnostics. Only a failed build is worth
	// reporting, and then the whole log is useful.
	var out, buildLog bytes.Buffer
	cmd.Stdout = &out
	cmd.Stderr = &buildLog
	if err := cmd.Run(); err != nil {
		if _, werr := stderr.Write(buildLog.Bytes()); werr != nil {
			return "", fmt.Errorf("cargo build in %s failed: %w", dir, err)
		}
		return "", fmt.Errorf("cargo build in %s failed: %w", dir, err)
	}

	// cargo emits one JSON object per line; the executable belongs to the
	// last compiler-artifact that produced one.
	var exe string
	for _, line := range strings.Split(out.String(), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		var msg struct {
			Reason     string  `json:"reason"`
			Executable *string `json:"executable"`
		}
		if err := json.Unmarshal([]byte(line), &msg); err != nil {
			continue
		}
		if msg.Reason == "compiler-artifact" && msg.Executable != nil && *msg.Executable != "" {
			exe = *msg.Executable
		}
	}
	if exe == "" {
		return "", fmt.Errorf("cargo build in %s produced no executable", dir)
	}
	return exe, nil
}

// cargoManifest is a decoded Cargo.toml.
//
// The manifest is read as the TOML it is rather than scanned line by line.
// Cargo accepts far more shapes than a scanner can follow — `path="../sdk"`
// without the spaces, a `[dependencies.pulumi_aws]` sub-table, a
// `[target.'cfg(unix)'.dependencies]` table, an inline table spread over
// several lines — and every shape a scanner misses is a dependency that
// silently disappears. When that dependency is a provider SDK the engine
// never installs the plugin, and the update fails much later with an
// unrelated "no resource plugin found".
type cargoManifest map[string]any

// readCargoManifest decodes dir/Cargo.toml.
func readCargoManifest(dir string) (cargoManifest, error) {
	path := filepath.Join(dir, "Cargo.toml")
	contents, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", path, err)
	}
	var manifest cargoManifest
	if _, err := toml.Decode(string(contents), &manifest); err != nil {
		return nil, fmt.Errorf("parsing %s: %w", path, err)
	}
	return manifest, nil
}

// table walks a chain of nested tables, returning nil if any step is absent
// or is not a table. The nil is a usable empty map, so callers need not
// check it before indexing.
func (m cargoManifest) table(path ...string) map[string]any {
	current := map[string]any(m)
	for _, step := range path {
		next, ok := current[step].(map[string]any)
		if !ok {
			return nil
		}
		current = next
	}
	return current
}

// dependencyTables returns every table whose entries name a dependency of
// the crate itself: `[dependencies]`, its per-target forms under
// `[target.<cfg>.dependencies]`, and `[workspace.dependencies]` — the last
// because that is where a member's `foo = { workspace = true }` entry
// actually resolves. dev- and build-dependencies are deliberately excluded:
// neither is linked into the program the engine runs.
func (m cargoManifest) dependencyTables() []map[string]any {
	tables := []map[string]any{
		m.table("dependencies"),
		m.table("workspace", "dependencies"),
	}
	targets := make([]string, 0, len(m.table("target")))
	for cfg := range m.table("target") {
		targets = append(targets, cfg)
	}
	sort.Strings(targets)
	for _, cfg := range targets {
		tables = append(tables, m.table("target", cfg, "dependencies"))
	}
	return tables
}

// pathDependency describes one local path dependency of a program.
type pathDependency struct {
	// The Cargo dependency key (crate name).
	crateName string
	// Absolute path of the dependency.
	path string
}

// readPathDependencies parses a Cargo.toml and returns its local path
// dependencies, ordered by crate name so the answer does not depend on TOML
// table iteration order.
func readPathDependencies(programDir string) ([]pathDependency, error) {
	manifest, err := readCargoManifest(programDir)
	if err != nil {
		return nil, err
	}
	var deps []pathDependency
	seen := map[string]bool{}
	for _, table := range manifest.dependencyTables() {
		for crate, spec := range table {
			// A bare `crate = "1.0"` is a registry version requirement, not a
			// local path; only the table form can carry one.
			fields, ok := spec.(map[string]any)
			if !ok || seen[crate] {
				continue
			}
			depPath, ok := fields["path"].(string)
			if !ok || depPath == "" {
				continue
			}
			seen[crate] = true
			if !filepath.IsAbs(depPath) {
				depPath = filepath.Join(programDir, depPath)
			}
			deps = append(deps, pathDependency{crateName: crate, path: depPath})
		}
	}
	sort.Slice(deps, func(i, j int) bool { return deps[i].crateName < deps[j].crateName })
	return deps, nil
}

// pulumiPluginJSON mirrors the pulumi-plugin.json metadata emitted into
// generated SDKs.
type pulumiParameterizationJSON struct {
	Name    string `json:"name"`
	Version string `json:"version"`
	Value   []byte `json:"value"`
}

type pulumiPluginJSON struct {
	Resource bool   `json:"resource"`
	Name     string `json:"name"`
	Version  string `json:"version"`
	Server   string `json:"server,omitempty"`
	// A parameterized package names its BASE plugin above and carries the
	// parameter that turns it into the package the program uses.
	Parameterization          *pulumiParameterizationJSON `json:"parameterization,omitempty"`
	ExtensionParameterization *pulumiParameterizationJSON `json:"extensionParameterization,omitempty"`
}

func readPluginJSON(dir string) (*pulumiPluginJSON, error) {
	contents, err := os.ReadFile(filepath.Join(dir, "pulumi-plugin.json"))
	if err != nil {
		return nil, err
	}
	var pj pulumiPluginJSON
	if err := json.Unmarshal(contents, &pj); err != nil {
		return nil, err
	}
	return &pj, nil
}

// readPackageField reads a string field of the `[package]` table of the
// crate at dir.
func readPackageField(dir, field string) (string, error) {
	manifest, err := readCargoManifest(dir)
	if err != nil {
		return "", err
	}
	if value, ok := manifest.table("package")[field].(string); ok && value != "" {
		return value, nil
	}
	return "", fmt.Errorf("no %s in %s", field, filepath.Join(dir, "Cargo.toml"))
}

// readCrateVersion reads the version of the crate at dir from its manifest.
func readCrateVersion(dir string) (string, error) {
	return readPackageField(dir, "version")
}

// readCrateName reads the crate name at dir from its manifest.
func readCrateName(dir string) (string, error) {
	return readPackageField(dir, "name")
}

func (host *rustLanguageHost) GetProgramDependencies(
	ctx context.Context, req *pulumirpc.GetProgramDependenciesRequest,
) (*pulumirpc.GetProgramDependenciesResponse, error) {
	deps, err := readPathDependencies(req.GetInfo().GetProgramDirectory())
	if err != nil {
		return nil, err
	}
	var out []*pulumirpc.DependencyInfo
	for _, dep := range deps {
		if pj, err := readPluginJSON(dep.path); err == nil {
			// A parameterized package's plugin JSON names the base plugin;
			// the dependency the program actually uses is the parameterized
			// package itself.
			version := pj.Version
			if p := pj.Parameterization; p != nil {
				version = p.Version
			} else if p := pj.ExtensionParameterization; p != nil {
				version = p.Version
			}
			out = append(out, &pulumirpc.DependencyInfo{Name: dep.crateName, Version: version})
			continue
		}
		version, err := readCrateVersion(dep.path)
		if err != nil {
			version = ""
		}
		out = append(out, &pulumirpc.DependencyInfo{Name: dep.crateName, Version: version})
	}
	return &pulumirpc.GetProgramDependenciesResponse{Dependencies: out}, nil
}

func (host *rustLanguageHost) GetRequiredPackages(
	ctx context.Context, req *pulumirpc.GetRequiredPackagesRequest,
) (*pulumirpc.GetRequiredPackagesResponse, error) {
	deps, err := readPathDependencies(req.GetInfo().GetProgramDirectory())
	if err != nil {
		return nil, err
	}
	var packages []*pulumirpc.PackageDependency
	for _, dep := range deps {
		pj, err := readPluginJSON(dep.path)
		if err != nil || !pj.Resource {
			continue
		}
		dep := &pulumirpc.PackageDependency{
			Name:    pj.Name,
			Kind:    "resource",
			Version: pj.Version,
			Server:  pj.Server,
		}
		if p := pj.Parameterization; p != nil {
			dep.Parameterization = &pulumirpc.PackageParameterization{
				Name:    p.Name,
				Version: p.Version,
				Value:   p.Value,
			}
		}
		if p := pj.ExtensionParameterization; p != nil {
			dep.Extension = &pulumirpc.PackageParameterization{
				Name:    p.Name,
				Version: p.Version,
				Value:   p.Value,
			}
		}
		packages = append(packages, dep)
	}
	return &pulumirpc.GetRequiredPackagesResponse{Packages: packages}, nil
}

// Pack copies a Rust SDK crate into the destination directory. Cargo
// consumes SDKs as path dependencies, so the artifact is a directory.
func (host *rustLanguageHost) Pack(ctx context.Context, req *pulumirpc.PackRequest) (*pulumirpc.PackResponse, error) {
	name, err := readCrateName(req.PackageDirectory)
	if err != nil {
		return nil, err
	}
	version, err := readCrateVersion(req.PackageDirectory)
	if err != nil {
		return nil, err
	}
	dest := filepath.Join(req.DestinationDirectory, fmt.Sprintf("%s-%s", name, version))

	if err := os.RemoveAll(dest); err != nil {
		return nil, err
	}
	if err := copyCrate(req.PackageDirectory, dest); err != nil {
		return nil, fmt.Errorf("copying crate: %w", err)
	}
	return &pulumirpc.PackResponse{ArtifactPath: dest}, nil
}

// copyCrate copies a crate's source, skipping build artifacts.
func copyCrate(src, dst string) error {
	return filepath.Walk(src, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return os.MkdirAll(dst, 0o755)
		}
		base := filepath.Base(rel)
		if info.IsDir() {
			if base == "target" || base == ".git" {
				return filepath.SkipDir
			}
			return os.MkdirAll(filepath.Join(dst, rel), 0o755)
		}
		if base == "Cargo.lock" {
			return nil
		}
		contents, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		return os.WriteFile(filepath.Join(dst, rel), contents, 0o644)
	})
}

func (host *rustLanguageHost) GeneratePackage(
	ctx context.Context, req *pulumirpc.GeneratePackageRequest,
) (*pulumirpc.GeneratePackageResponse, error) {
	loader, err := schema.NewLoaderClient(req.LoaderTarget)
	if err != nil {
		return nil, err
	}
	defer loader.Close()

	var spec schema.PackageSpec
	if err := json.Unmarshal([]byte(req.Schema), &spec); err != nil {
		return nil, err
	}
	pkg, diags, err := schema.BindSpec(spec, loader, schema.ValidationOptions{
		AllowDanglingReferences: true,
	})
	if err != nil {
		return nil, err
	}
	rpcDiagnostics := plugin.HclDiagnosticsToRPCDiagnostics(diags)
	if diags.HasErrors() {
		return &pulumirpc.GeneratePackageResponse{Diagnostics: rpcDiagnostics}, nil
	}
	files, err := codegen.GeneratePackage("pulumi-language-rust", pkg, req.ExtraFiles, req.LocalDependencies)
	if err != nil {
		return nil, err
	}
	for filename, data := range files {
		outPath := filepath.Join(req.Directory, filename)
		if err := os.MkdirAll(filepath.Dir(outPath), 0o700); err != nil {
			return nil, err
		}
		if err := os.WriteFile(outPath, data, 0o600); err != nil {
			return nil, err
		}
	}
	return &pulumirpc.GeneratePackageResponse{Diagnostics: rpcDiagnostics}, nil
}

func (host *rustLanguageHost) GenerateProject(
	ctx context.Context, req *pulumirpc.GenerateProjectRequest,
) (*pulumirpc.GenerateProjectResponse, error) {
	loader, err := schema.NewLoaderClient(req.LoaderTarget)
	if err != nil {
		return nil, err
	}
	defer loader.Close()

	var extraOptions []pcl.BindOption
	if !req.Strict {
		extraOptions = append(extraOptions, pcl.NonStrictBindOptions()...)
	}
	program, diags, err := pcl.BindDirectory(req.SourceDirectory, schema.NewCachedLoader(loader), extraOptions...)
	if err != nil {
		return nil, err
	}
	rpcDiagnostics := plugin.HclDiagnosticsToRPCDiagnostics(diags)
	if diags.HasErrors() {
		return &pulumirpc.GenerateProjectResponse{Diagnostics: rpcDiagnostics}, nil
	}

	var project workspace.Project
	if err := json.Unmarshal([]byte(req.Project), &project); err != nil {
		return nil, err
	}

	err = codegen.GenerateProject(req.TargetDirectory, project, program, req.LocalDependencies)
	if err != nil {
		return nil, err
	}
	return &pulumirpc.GenerateProjectResponse{Diagnostics: rpcDiagnostics}, nil
}

func (host *rustLanguageHost) GenerateProgram(
	ctx context.Context, req *pulumirpc.GenerateProgramRequest,
) (*pulumirpc.GenerateProgramResponse, error) {
	loader, err := schema.NewLoaderClient(req.LoaderTarget)
	if err != nil {
		return nil, err
	}
	defer loader.Close()

	parser := syntax.NewParser()
	for path, contents := range req.Source {
		if err := parser.ParseFile(strings.NewReader(contents), path); err != nil {
			return nil, err
		}
	}
	var bindOptions []pcl.BindOption
	if !req.Strict {
		bindOptions = append(bindOptions, pcl.NonStrictBindOptions()...)
	}
	program, diags, err := pcl.BindProgram(parser.Files, schema.NewCachedLoader(loader), bindOptions...)
	if err != nil {
		return nil, err
	}
	rpcDiagnostics := plugin.HclDiagnosticsToRPCDiagnostics(diags)
	if diags.HasErrors() {
		return &pulumirpc.GenerateProgramResponse{Diagnostics: rpcDiagnostics}, nil
	}
	files, genDiags, err := codegen.GenerateProgram(program)
	if err != nil {
		return nil, err
	}
	rpcDiagnostics = append(rpcDiagnostics, plugin.HclDiagnosticsToRPCDiagnostics(genDiags)...)
	return &pulumirpc.GenerateProgramResponse{Source: files, Diagnostics: rpcDiagnostics}, nil
}
