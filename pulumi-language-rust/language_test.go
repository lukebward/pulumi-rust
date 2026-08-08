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

package main

import (
	"bufio"
	"fmt"
	"io"
	"os/exec"
	"strings"
	"sync"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"

	"github.com/pulumi/pulumi/sdk/v3/go/common/util/contract"
	"github.com/pulumi/pulumi/sdk/v3/go/common/util/rpcutil"
	pulumirpc "github.com/pulumi/pulumi/sdk/v3/proto/go"
	testingrpc "github.com/pulumi/pulumi/sdk/v3/proto/go/testing"
)

// expectedFailures maps conformance test names to the reason they are
// currently skipped. Entries here are honest debt: each one is a feature the
// Rust language implementation does not support yet.
var expectedFailures = map[string]string{

	// try/can/recover builtins need error-tracking through expression
	// evaluation, which the dynamic evaluator does not model yet.
	"l2-failed-create-recover-continue-on-error": "the recover() builtin is not implemented",

	// Resource methods (call) are not implemented in codegen or the SDK.
	"l2-component-call-simple":  "resource methods (call) are not implemented",
	"l2-component-call-plain":   "resource methods (call) are not implemented",
	"l2-provider-call":          "resource methods (call) are not implemented",
	"l2-provider-call-explicit": "resource methods (call) are not implemented",
	"l2-index-mod":              "resource methods (call) are not implemented",
	"l2-module-format":          "resource methods (call) are not implemented",

	// Package parameterization (RegisterPackage + parameterized SDKs) is
	// not implemented.
	"l2-parameterized-invoke":             "package parameterization is not implemented",
	"l2-parameterized-resource":           "package parameterization is not implemented",
	"l2-parameterized-resource-twice":     "package parameterization is not implemented",
	"l2-explicit-parameterized-provider":  "package parameterization is not implemented",
	"l2-extension-parameterized-resource": "package parameterization is not implemented",
	"l2-extension-and-base-resource":      "package parameterization is not implemented",

	// Package namespaces are not implemented.
	"l2-namespaced-provider": "package namespaces are not implemented",

	// Reading through resource-reference outputs requires hydration via
	// pulumi:pulumi:getResource.
	"l2-component-component-resource-ref": "resource reference hydration is not implemented",
	"l2-component-program-resource-ref":   "resource reference hydration is not implemented",
	"l2-component-property-deps":          "component property dependencies are not implemented",

	// Resource lifecycle hooks are not implemented.
	"l2-resource-hook-after-failure": "resource hooks are not implemented",
	"l2-resource-hook-ignore-errors": "resource hooks are not implemented",
	"l2-resource-hook-on-error":      "resource hooks are not implemented",
	"l2-resource-option-hooks":       "resource hooks are not implemented",


	// Local (in-language) components are not implemented in programgen.
	"l3-component-simple":                "local components are not implemented",
	"l3-component-nested":                "local components are not implemented",
	"l3-component-invoke":                "local components are not implemented",
	"l3-component-config-objects":        "local components are not implemented",
	"l3-component-config-primitives":     "local components are not implemented",
	"l3-component-primitive-conversions": "local components are not implemented",
	"l3-component-provider":              "local components are not implemented",
	"l3-component-provider-inheritance":  "local components are not implemented",
	"l3-deferred-outputs":                "local components are not implemented",
	"l3-resource-keyword-overlap":        "local components are not implemented",
	"l3-rewrite-conversions":             "local components are not implemented",

}

func runTestingHost(t *testing.T) (string, testingrpc.LanguageTestClient) {
	// We can't just go run the pulumi-test-language package because of
	// https://github.com/golang/go/issues/39172, so we build it to a temp
	// file, then run that.
	binary := t.TempDir() + "/pulumi-test-language"
	cmd := exec.CommandContext(t.Context(),
		"go", "build", "-o", binary, "github.com/pulumi/pulumi/pkg/v3/testing/pulumi-test-language")
	output, err := cmd.CombinedOutput()
	t.Logf("build output: %s", output)
	require.NoError(t, err)

	cmd = exec.Command(binary)
	stdout, err := cmd.StdoutPipe()
	require.NoError(t, err)
	stderr, err := cmd.StderrPipe()
	require.NoError(t, err)
	stderrReader := bufio.NewReader(stderr)

	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		for {
			text, err := stderrReader.ReadString('\n')
			if err != nil {
				break
			}
			t.Logf("engine: %s", text)
		}
	}()

	err = cmd.Start()
	require.NoError(t, err)

	stdoutBytes, err := io.ReadAll(stdout)
	require.NoError(t, err)
	address := string(stdoutBytes)

	conn, err := grpc.NewClient(
		address,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		rpcutil.GrpcChannelOptions(),
	)
	require.NoError(t, err)
	client := testingrpc.NewLanguageTestClient(conn)

	t.Cleanup(func() {
		assert.NoError(t, cmd.Process.Kill())
		wg.Wait()
		// We expect this to error because we just killed it.
		contract.IgnoreError(cmd.Wait())
	})
	return address, client
}

func TestLanguage(t *testing.T) {
	t.Parallel()

	engineAddress, engine := runTestingHost(t)

	tests, err := engine.GetLanguageTests(t.Context(), &testingrpc.GetLanguageTestsRequest{})
	require.NoError(t, err)

	cancel := make(chan bool)

	// Run the language plugin in-process.
	handle, err := rpcutil.ServeWithOptions(rpcutil.ServeOptions{
		Init: func(srv *grpc.Server) error {
			host := newLanguageHost(engineAddress)
			pulumirpc.RegisterLanguageRuntimeServer(srv, host)
			return nil
		},
		Cancel: cancel,
	})
	require.NoError(t, err)

	// A temp directory for all test projects and artifacts.
	rootDir := t.TempDir()

	snapshotDir := "./testdata/"

	prepare, err := engine.PrepareLanguageTests(t.Context(), &testingrpc.PrepareLanguageTestsRequest{
		LanguagePluginName:   "rust",
		LanguagePluginTarget: fmt.Sprintf("127.0.0.1:%d", handle.Port),
		TemporaryDirectory:   rootDir,
		SnapshotDirectory:    snapshotDir,
		CoreSdkDirectory:     "../sdk/rust/pulumi",
		CoreSdkVersion:       "0.1.0",
		SnapshotEdits: []*testingrpc.PrepareLanguageTestsRequest_Replacement{
			{
				Pattern:     rootDir + "/artifacts",
				Replacement: "ROOT/artifacts",
			},
		},
	})
	require.NoError(t, err)

	for _, tt := range tests.Tests {
		tt := tt
		t.Run(tt, func(t *testing.T) {
			t.Parallel()
			if expected, ok := expectedFailures[tt]; ok {
				t.Skipf("test %s is expected to fail: %s", tt, expected)
			}
			if strings.HasPrefix(tt, "policy-") {
				t.Skipf("rust doesn't support policy tests yet: %s", tt)
			}
			if strings.HasPrefix(tt, "provider-") {
				t.Skipf("rust doesn't support provider tests yet: %s", tt)
			}

			result, err := engine.RunLanguageTest(t.Context(), &testingrpc.RunLanguageTestRequest{
				Token: prepare.Token,
				Test:  tt,
			})

			require.NoError(t, err)
			for _, msg := range result.Messages {
				t.Log(msg)
			}
			t.Logf("stdout: %s", result.Stdout)
			t.Logf("stderr: %s", result.Stderr)
			assert.True(t, result.Success)
		})
	}

	t.Cleanup(func() {
		close(cancel)
		assert.NoError(t, <-handle.Done)
	})
}
