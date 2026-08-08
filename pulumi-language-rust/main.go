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
	"flag"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	pbempty "google.golang.org/protobuf/types/known/emptypb"

	"google.golang.org/grpc"

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

	handle, err := rpcutil.ServeWithOptions(rpcutil.ServeOptions{
		Cancel: cancelChannel,
		Init: func(srv *grpc.Server) error {
			host := newLanguageHost(engineAddress)
			pulumirpc.RegisterLanguageRuntimeServer(srv, host)
			return nil
		},
		Options: rpcutil.OpenTracingServerInterceptorOptions(nil),
	})
	if err != nil {
		cmdutil.Exit(fmt.Errorf("could not start language host RPC server: %w", err))
	}

	fmt.Printf("%d\n", handle.Port)

	if err := <-handle.Done; err != nil {
		cmdutil.Exit(fmt.Errorf("language host RPC stopped serving: %w", err))
	}
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
func sharedTargetDir() string {
	return filepath.Join(os.TempDir(), fmt.Sprintf("pulumi-language-rust-target-%d", os.Getuid()))
}

func cargoCommand(ctx context.Context, dir string, extraEnv []string, args ...string) (*exec.Cmd, error) {
	cargo, err := executable.FindExecutable("cargo")
	if err != nil {
		return nil, fmt.Errorf("could not find cargo on PATH: %w", err)
	}
	cmd := exec.CommandContext(ctx, cargo, args...)
	cmd.Dir = dir
	cmd.Env = append(os.Environ(),
		"CARGO_TARGET_DIR="+sharedTargetDir(),
		// Debug info dominates build-artifact size; conformance runs build
		// hundreds of crates, so keep artifacts lean.
		"CARGO_PROFILE_DEV_DEBUG=false",
	)
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

func asExitError(err error, target **exec.ExitError) bool {
	if ee, ok := err.(*exec.ExitError); ok {
		*target = ee
		return true
	}
	return false
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

// rewritePathDependency repoints one dependency's path, adding the
// dependency when the manifest does not already name it.
func rewritePathDependency(manifest, crate, path string) string {
	lines := strings.Split(manifest, "\n")
	prefix := crate + " ="
	for i, line := range lines {
		if strings.HasPrefix(strings.TrimSpace(line), prefix) {
			lines[i] = fmt.Sprintf("%s = { path = %q }", crate, path)
			return strings.Join(lines, "\n")
		}
	}
	// Not present: append under [dependencies], or start that section.
	for i, line := range lines {
		if strings.TrimSpace(line) == "[dependencies]" {
			entry := fmt.Sprintf("%s = { path = %q }", crate, path)
			lines = append(lines[:i+1], append([]string{entry}, lines[i+1:]...)...)
			return strings.Join(lines, "\n")
		}
	}
	return manifest + fmt.Sprintf("\n[dependencies]\n%s = { path = %q }\n", crate, path)
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

	// Build first so cargo's own progress never reaches stdout ahead of the
	// port line.
	build, err := cargoCommand(server.Context(), dir, req.GetEnv(), "build", "--quiet")
	if err != nil {
		return err
	}
	build.Stdout = stderr
	build.Stderr = stderr
	if err := build.Run(); err != nil {
		return fmt.Errorf("cargo build in %s failed: %w", dir, err)
	}

	args := append([]string{"run", "--quiet", "--"}, req.GetArgs()...)
	cmd, err := cargoCommand(server.Context(), dir, req.GetEnv(), args...)
	if err != nil {
		return err
	}
	cmd.Stdout = stdout
	cmd.Stderr = stderr
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

// pathDependency describes one local path dependency of a program.
type pathDependency struct {
	// The Cargo dependency key (crate name).
	crateName string
	// Absolute path of the dependency.
	path string
}

// readPathDependencies parses a Cargo.toml and returns its local path
// dependencies.
func readPathDependencies(programDir string) ([]pathDependency, error) {
	manifest := filepath.Join(programDir, "Cargo.toml")
	contents, err := os.ReadFile(manifest)
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", manifest, err)
	}
	var deps []pathDependency
	inDeps := false
	for _, line := range strings.Split(string(contents), "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") {
			inDeps = trimmed == "[dependencies]"
			continue
		}
		if !inDeps || trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}
		name, rest, found := strings.Cut(trimmed, "=")
		if !found {
			continue
		}
		name = strings.TrimSpace(name)
		idx := strings.Index(rest, "path =")
		if idx < 0 {
			continue
		}
		pathPart := rest[idx+len("path ="):]
		pathPart = strings.TrimSpace(pathPart)
		if !strings.HasPrefix(pathPart, "\"") {
			continue
		}
		end := strings.Index(pathPart[1:], "\"")
		if end < 0 {
			continue
		}
		depPath := pathPart[1 : 1+end]
		if !filepath.IsAbs(depPath) {
			depPath = filepath.Join(programDir, depPath)
		}
		deps = append(deps, pathDependency{crateName: name, path: depPath})
	}
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

// readCrateVersion reads the version of the crate at dir from its manifest.
func readCrateVersion(dir string) (string, error) {
	contents, err := os.ReadFile(filepath.Join(dir, "Cargo.toml"))
	if err != nil {
		return "", err
	}
	inPackage := false
	for _, line := range strings.Split(string(contents), "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") {
			inPackage = trimmed == "[package]"
			continue
		}
		if !inPackage {
			continue
		}
		if name, rest, ok := strings.Cut(trimmed, "="); ok && strings.TrimSpace(name) == "version" {
			return strings.Trim(strings.TrimSpace(rest), "\""), nil
		}
	}
	return "", fmt.Errorf("no version in %s/Cargo.toml", dir)
}

// readCrateName reads the crate name at dir from its manifest.
func readCrateName(dir string) (string, error) {
	contents, err := os.ReadFile(filepath.Join(dir, "Cargo.toml"))
	if err != nil {
		return "", err
	}
	inPackage := false
	for _, line := range strings.Split(string(contents), "\n") {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "[") {
			inPackage = trimmed == "[package]"
			continue
		}
		if !inPackage {
			continue
		}
		if name, rest, ok := strings.Cut(trimmed, "="); ok && strings.TrimSpace(name) == "name" {
			return strings.Trim(strings.TrimSpace(rest), "\""), nil
		}
	}
	return "", fmt.Errorf("no name in %s/Cargo.toml", dir)
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
