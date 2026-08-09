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
