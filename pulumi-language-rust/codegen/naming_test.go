package codegen

import "testing"

func TestSnakeCaseDropsNonIdentifierRunes(t *testing.T) {
	cases := map[string]string{
		"$ref":     "ref",
		"$schema":  "schema",
		"someName": "some_name",
		"HTTPPort": "httpport",
		"a1b":      "a1b",
		"1abc":     "_1abc",
	}
	for in, want := range cases {
		if got := snakeCase(in); got != want {
			t.Errorf("snakeCase(%q) = %q, want %q", in, got, want)
		}
	}
}
