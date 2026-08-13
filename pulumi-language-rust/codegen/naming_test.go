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

package codegen

import "testing"

// The identifier-shaping rules, as distinct from word breaking (which
// TestSnakeCaseWordBreaking covers): runes that cannot appear in a Rust
// identifier are dropped rather than turned into separators, and an
// identifier cannot begin with a digit.
func TestSnakeCaseDropsNonIdentifierRunes(t *testing.T) {
	cases := map[string]string{
		"$ref":     "ref",
		"$schema":  "schema",
		"someName": "some_name",
		"a1b":      "a1b",
		"1abc":     "_1abc",
	}
	for in, want := range cases {
		if got := snakeCase(in); got != want {
			t.Errorf("snakeCase(%q) = %q, want %q", in, got, want)
		}
	}
}
