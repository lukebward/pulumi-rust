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

// Package version carries the language host's version.
//
// The name shadows the standard library's go/version, which revive objects
// to. pulumi-language-dotnet's equivalent package suppresses the same rule
// for the same reason: every Pulumi language host calls this package
// `version`, and matching them is worth more than avoiding the shadow.
package version //nolint:revive // Deliberately named to match every other Pulumi language host.

// Version is initialized by the Go linker to contain the semver of this build.
//
// It is empty in a plain `go build`, which is what an unreleased build should
// report. The Makefile and .goreleaser.yml both stamp it with
// -X <this package>/version.Version=<semver>; the symbol path in those flags
// has to match this package's import path exactly, because the linker silently
// ignores a -X it cannot resolve.
var Version string
