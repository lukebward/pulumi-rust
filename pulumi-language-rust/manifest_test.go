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
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	pulumirpc "github.com/pulumi/pulumi/sdk/v3/proto/go"
)

// writeManifest drops a Cargo.toml in a fresh directory and returns it.
func writeManifest(t *testing.T, contents string) string {
	t.Helper()
	dir := t.TempDir()
	require.NoError(t, os.WriteFile(filepath.Join(dir, "Cargo.toml"), []byte(contents), 0o600))
	return dir
}

// Cargo accepts a great many spellings for the same dependency. Every one a
// reader misses is a provider SDK that vanishes from GetRequiredPackages,
// leaving the engine to skip the plugin install and fail the update later
// with an unrelated "no resource plugin found".
func TestReadPathDependencies(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name     string
		manifest string
		want     []pathDependency
	}{
		{
			name:     "the plain inline table the generator emits",
			manifest: "[package]\nname = \"prog\"\n\n[dependencies]\npulumi = { path = \"../sdk\" }\n",
			want:     []pathDependency{{crateName: "pulumi", path: "../sdk"}},
		},
		{
			name:     "no spaces around the path assignment",
			manifest: "[dependencies]\npulumi_aws = { path=\"../sdk\" }\n",
			want:     []pathDependency{{crateName: "pulumi_aws", path: "../sdk"}},
		},
		{
			name:     "a dependency sub-table",
			manifest: "[dependencies.pulumi_aws]\nversion = \"6.0\"\npath = \"../sdk\"\n",
			want:     []pathDependency{{crateName: "pulumi_aws", path: "../sdk"}},
		},
		{
			name:     "a target-specific dependency table",
			manifest: "[target.'cfg(unix)'.dependencies]\npulumi_aws = { path = \"../sdk\" }\n",
			want:     []pathDependency{{crateName: "pulumi_aws", path: "../sdk"}},
		},
		{
			name:     "an inline table spread over several lines",
			manifest: "[dependencies]\npulumi_aws = {\n  version = \"6.0\",\n  path = \"../sdk\",\n}\n",
			want:     []pathDependency{{crateName: "pulumi_aws", path: "../sdk"}},
		},
		{
			name: "a workspace member resolves through the shared table",
			manifest: "[workspace.dependencies]\npulumi = { path = \"../sdk\" }\n\n" +
				"[dependencies]\npulumi = { workspace = true }\n",
			want: []pathDependency{{crateName: "pulumi", path: "../sdk"}},
		},
		{
			name:     "a registry version requirement is not a path dependency",
			manifest: "[dependencies]\npulumi = \"0.1\"\nserde = { version = \"1\", features = [\"derive\"] }\n",
			want:     nil,
		},
		{
			name: "dev- and build-dependencies are not part of the program",
			manifest: "[dev-dependencies]\npulumi_test = { path = \"../test\" }\n\n" +
				"[build-dependencies]\npulumi_build = { path = \"../build\" }\n",
			want: nil,
		},
		{
			name:     "comments and blank lines are ignored",
			manifest: "# the program\n[dependencies] # deps\n\n# our SDK\npulumi = { path = \"../sdk\" } # local\n",
			want:     []pathDependency{{crateName: "pulumi", path: "../sdk"}},
		},
		{
			name: "several dependencies come back in a stable order",
			manifest: "[dependencies]\npulumi_simple = { path = \"b\" }\npulumi = { path = \"a\" }\n" +
				"pulumi_other = { path = \"c\" }\n",
			want: []pathDependency{
				{crateName: "pulumi", path: "a"},
				{crateName: "pulumi_other", path: "c"},
				{crateName: "pulumi_simple", path: "b"},
			},
		},
		{
			name:     "a quoted dependency key keeps its name",
			manifest: "[dependencies]\n\"pulumi-aws\" = { path = \"../sdk\" }\n",
			want:     []pathDependency{{crateName: "pulumi-aws", path: "../sdk"}},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			dir := writeManifest(t, tt.manifest)
			got, err := readPathDependencies(dir)
			require.NoError(t, err)

			var want []pathDependency
			for _, dep := range tt.want {
				// Relative dependency paths resolve against the program.
				want = append(want, pathDependency{crateName: dep.crateName, path: filepath.Join(dir, dep.path)})
			}
			assert.Equal(t, want, got)
		})
	}
}

func TestReadPathDependenciesAbsolutePath(t *testing.T) {
	t.Parallel()

	// An artifact path handed over by the engine is already absolute and
	// must not be re-rooted at the program directory.
	abs := filepath.Join(string(filepath.Separator), "artifacts", "pulumi-0.1.0")
	dir := writeManifest(t, "[dependencies]\npulumi = { path = "+quoteTOML(abs)+" }\n")
	got, err := readPathDependencies(dir)
	require.NoError(t, err)
	assert.Equal(t, []pathDependency{{crateName: "pulumi", path: abs}}, got)
}

func TestReadPathDependenciesRejectsBrokenManifest(t *testing.T) {
	t.Parallel()

	// Cargo would refuse this too. Reporting it beats reporting no
	// dependencies, which reads to the engine as a program needing no
	// plugins at all.
	dir := writeManifest(t, "[dependencies\npulumi = { path = \"../sdk\" }\n")
	_, err := readPathDependencies(dir)
	require.Error(t, err)
	assert.Contains(t, err.Error(), "Cargo.toml")
}

func TestReadCrateNameAndVersion(t *testing.T) {
	t.Parallel()

	t.Run("a trailing comment is not part of the value", func(t *testing.T) {
		t.Parallel()
		dir := writeManifest(t,
			"[package]\nname = \"demo\" # the crate\nversion = \"1.2.3\" # keep in sync\nedition = \"2021\"\n")
		name, err := readCrateName(dir)
		require.NoError(t, err)
		assert.Equal(t, "demo", name)
		version, err := readCrateVersion(dir)
		require.NoError(t, err)
		assert.Equal(t, "1.2.3", version)
	})

	t.Run("a package sub-table does not shadow the package fields", func(t *testing.T) {
		t.Parallel()
		dir := writeManifest(t,
			"[package.metadata.docs.rs]\nname = \"docs\"\nversion = \"9.9.9\"\n\n"+
				"[package]\nname = \"demo\"\nversion = \"1.2.3\"\n")
		name, err := readCrateName(dir)
		require.NoError(t, err)
		assert.Equal(t, "demo", name)
		version, err := readCrateVersion(dir)
		require.NoError(t, err)
		assert.Equal(t, "1.2.3", version)
	})

	t.Run("a manifest with no package table is an error", func(t *testing.T) {
		t.Parallel()
		dir := writeManifest(t, "[workspace]\nmembers = [\"a\"]\n")
		_, err := readCrateName(dir)
		require.Error(t, err)
		_, err = readCrateVersion(dir)
		require.Error(t, err)
	})
}

// The whole point of reading the manifest: a provider SDK reached through a
// shape the old line scanner could not see still gets its plugin installed.
func TestGetRequiredPackagesFindsSubTableDependency(t *testing.T) {
	t.Parallel()

	root := t.TempDir()
	sdk := filepath.Join(root, "sdk")
	require.NoError(t, os.MkdirAll(sdk, 0o700))
	require.NoError(t, os.WriteFile(filepath.Join(sdk, "pulumi-plugin.json"),
		[]byte(`{"resource":true,"name":"aws","version":"6.0.0"}`), 0o600))

	program := filepath.Join(root, "program")
	require.NoError(t, os.MkdirAll(program, 0o700))
	require.NoError(t, os.WriteFile(filepath.Join(program, "Cargo.toml"), []byte(
		"[package]\nname = \"prog\"\nversion = \"0.1.0\"\n\n"+
			"[dependencies]\npulumi = \"0.1\"\n\n"+
			"[dependencies.pulumi_aws]\npath=\"../sdk\"\n"), 0o600))

	host := newLanguageHost("")
	resp, err := host.GetRequiredPackages(t.Context(), &pulumirpc.GetRequiredPackagesRequest{
		Info: &pulumirpc.ProgramInfo{ProgramDirectory: program},
	})
	require.NoError(t, err)
	require.Len(t, resp.GetPackages(), 1)
	assert.Equal(t, "aws", resp.GetPackages()[0].GetName())
	assert.Equal(t, "6.0.0", resp.GetPackages()[0].GetVersion())
}

// quoteTOML renders a string as a TOML basic string.
func quoteTOML(s string) string {
	return `"` + strings.ReplaceAll(s, `\`, `\\`) + `"`
}
