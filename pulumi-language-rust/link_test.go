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

import "testing"

func TestRewritePathDependency(t *testing.T) {
	const path = "/artifacts/pulumi-0.1.0"
	want := `pulumi = { path = "/artifacts/pulumi-0.1.0" }`

	t.Run("only the dependencies table is rewritten", func(t *testing.T) {
		// A dev-dependency of the same name appears first; rewriting it
		// would leave the real dependency pointing at the registry.
		in := "[dev-dependencies]\npulumi = \"0.1\"\n\n[dependencies]\npulumi = \"0.1\"\n"
		got := rewritePathDependency(in, "pulumi", path)
		if got != "[dev-dependencies]\npulumi = \"0.1\"\n\n[dependencies]\n"+want+"\n" {
			t.Errorf("got:\n%s", got)
		}
	})

	t.Run("spacing variants are matched, not duplicated", func(t *testing.T) {
		for _, in := range []string{
			"[dependencies]\npulumi  = \"0.1\"\n",
			"[dependencies]\npulumi= \"0.1\"\n",
		} {
			got := rewritePathDependency(in, "pulumi", path)
			if got != "[dependencies]\n"+want+"\n" {
				t.Errorf("for %q got:\n%s", in, got)
			}
		}
	})

	t.Run("a similarly named crate is not mistaken for the target", func(t *testing.T) {
		in := "[dependencies]\npulumi_aws = \"1\"\n"
		got := rewritePathDependency(in, "pulumi", path)
		if got != "[dependencies]\n"+want+"\npulumi_aws = \"1\"\n" {
			t.Errorf("got:\n%s", got)
		}
	})

	t.Run("a multi-line inline table leaves no orphan", func(t *testing.T) {
		in := "[dependencies]\npulumi = {\n  version = \"0.1\"\n}\nother = \"2\"\n"
		got := rewritePathDependency(in, "pulumi", path)
		if got != "[dependencies]\n"+want+"\nother = \"2\"\n" {
			t.Errorf("got:\n%s", got)
		}
	})

	t.Run("the dotted table form is replaced", func(t *testing.T) {
		in := "[dependencies.pulumi]\nversion = \"0.1\"\n"
		got := rewritePathDependency(in, "pulumi", path)
		if got != "[dependencies]\n"+want+"\n" {
			t.Errorf("got:\n%s", got)
		}
	})

	t.Run("rewriting twice is a no-op", func(t *testing.T) {
		in := "[dependencies]\npulumi = \"0.1\"\n"
		once := rewritePathDependency(in, "pulumi", path)
		if twice := rewritePathDependency(once, "pulumi", path); twice != once {
			t.Errorf("not idempotent:\n%s", twice)
		}
	})

	t.Run("a missing dependency is added under the existing table", func(t *testing.T) {
		in := "[dependencies]\nother = \"2\"\n"
		got := rewritePathDependency(in, "pulumi", path)
		if got != "[dependencies]\n"+want+"\nother = \"2\"\n" {
			t.Errorf("got:\n%s", got)
		}
	})
}

func TestRewritePathDependencyWorkspace(t *testing.T) {
	// A workspace shares one entry across its members: the member's
	// `{ workspace = true }` indirection must survive, and the shared
	// `[workspace.dependencies]` entry is the one that gets repointed.
	in := "[workspace]\nmembers = [\"sdks/simple\"]\n\n" +
		"[workspace.dependencies]\npulumi = { path = \"../../sdk\" }\n\n" +
		"[dependencies]\npulumi = { workspace = true }\npulumi_simple = { path = \"sdks/simple\" }\n"
	got := rewritePathDependency(in, "pulumi", "/artifacts/pulumi-0.1.0")
	wantWs := `pulumi = { path = "/artifacts/pulumi-0.1.0" }`
	if !contains(got, "[workspace.dependencies]\n"+wantWs) {
		t.Errorf("workspace entry not repointed:\n%s", got)
	}
	if !contains(got, "pulumi = { workspace = true }") {
		t.Errorf("member indirection was clobbered:\n%s", got)
	}
}

func contains(haystack, needle string) bool {
	return len(haystack) >= len(needle) && (func() bool {
		for i := 0; i+len(needle) <= len(haystack); i++ {
			if haystack[i:i+len(needle)] == needle {
				return true
			}
		}
		return false
	})()
}
