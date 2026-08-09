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
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

// setUserCacheDir points every platform's notion of "the user's cache
// directory" at a directory this test owns, and returns it.
func setUserCacheDir(t *testing.T) string {
	t.Helper()
	cache := t.TempDir()
	t.Setenv("XDG_CACHE_HOME", cache) // Unix
	t.Setenv("HOME", cache)           // macOS, and the Unix fallback
	t.Setenv("LocalAppData", cache)   // Windows
	return cache
}

// The cargo target directory holds build-script binaries that cargo later
// executes. A predictable path under a world-writable /tmp is another local
// user's to claim — they can pre-create it, or point a symlink at a
// directory they control, before the victim's first run — so it belongs
// somewhere private to this user.
func TestSharedTargetDirIsPrivateToTheUser(t *testing.T) {
	cache := setUserCacheDir(t)

	dir := sharedTargetDir()
	require.NotEmpty(t, dir, "a cache directory was available, so a shared target dir should have been chosen")

	rel, err := filepath.Rel(cache, dir)
	require.NoError(t, err)
	assert.False(t, strings.HasPrefix(rel, ".."),
		"target dir %s should live under the user cache dir %s", dir, cache)

	// Specifically not the old world-guessable path.
	assert.NotEqual(t, filepath.Join(os.TempDir(), fmt.Sprintf("pulumi-language-rust-target-%d", os.Getuid())), dir)

	info, err := os.Stat(dir)
	require.NoError(t, err, "the target dir should have been created")
	require.True(t, info.IsDir())
	if runtime.GOOS != "windows" {
		assert.Equal(t, os.FileMode(0o700), info.Mode().Perm(),
			"the target dir should not be readable or writable by other users")
	}
}

// It is still one directory per machine, which is what keeps the
// conformance suite from recompiling the dependency graph once per test.
func TestSharedTargetDirIsStable(t *testing.T) {
	setUserCacheDir(t)
	assert.Equal(t, sharedTargetDir(), sharedTargetDir())
}

// With nowhere private to put it, cargo keeps its per-project default
// rather than falling back to a shared location.
func TestSharedTargetDirWithoutACacheDir(t *testing.T) {
	t.Setenv("XDG_CACHE_HOME", "")
	t.Setenv("HOME", "")
	t.Setenv("LocalAppData", "")
	if runtime.GOOS == "windows" || runtime.GOOS == "plan9" {
		t.Skip("os.UserCacheDir has other sources on this platform")
	}
	assert.Empty(t, sharedTargetDir())
}

// TestExitSeven is the helper subprocess for TestAsExitError; it is a no-op
// unless the parent asks it to exit.
func TestExitSeven(t *testing.T) {
	if os.Getenv("PULUMI_RUST_TEST_EXIT_SEVEN") == "1" {
		os.Exit(7)
	}
}

func TestAsExitError(t *testing.T) {
	t.Parallel()

	cmd := exec.CommandContext(t.Context(), os.Args[0], "-test.run=^TestExitSeven$")
	cmd.Env = append(os.Environ(), "PULUMI_RUST_TEST_EXIT_SEVEN=1")
	runErr := cmd.Run()
	require.Error(t, runErr)

	t.Run("a bare exit error", func(t *testing.T) {
		var exitErr *exec.ExitError
		require.True(t, asExitError(runErr, &exitErr))
		assert.Equal(t, 7, exitErr.ExitCode())
	})

	t.Run("an exit error wrapped in context", func(t *testing.T) {
		// Nothing on the way up is obliged to hand the error back bare, and
		// a program that exited 32 to say "already reported" must still be
		// read as such however it was wrapped.
		wrapped := fmt.Errorf("running plugin in %s: %w", "/some/dir", runErr)
		var exitErr *exec.ExitError
		require.True(t, asExitError(wrapped, &exitErr))
		assert.Equal(t, 7, exitErr.ExitCode())
	})

	t.Run("anything else is not an exit error", func(t *testing.T) {
		var exitErr *exec.ExitError
		assert.False(t, asExitError(errors.New("cargo not found"), &exitErr))
		assert.Nil(t, exitErr)
	})
}
