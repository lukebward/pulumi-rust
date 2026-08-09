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

// Package codegen generates Rust SDKs and programs from Pulumi schemas and
// PCL programs.
package codegen

import (
	"path/filepath"
	"strings"
	"unicode"
)

// rustKeywords are identifiers that need escaping in generated Rust code.
var rustKeywords = map[string]bool{
	"as": true, "async": true, "await": true, "break": true, "const": true,
	"continue": true, "dyn": true, "else": true, "enum": true,
	"extern": true, "false": true, "fn": true, "for": true, "if": true,
	"impl": true, "in": true, "let": true, "loop": true, "match": true,
	"mod": true, "move": true, "mut": true, "pub": true, "ref": true,
	"return": true, "static": true, "struct": true, "trait": true,
	"true": true, "type": true, "unsafe": true, "use": true, "where": true,
	"while": true, "abstract": true, "become": true, "box": true, "do": true,
	"final": true, "macro": true, "override": true, "priv": true,
	"typeof": true, "unsized": true, "virtual": true, "yield": true,
	"try": true, "gen": true, "union": true,
}

// rustNonRawKeywords cannot be raw identifiers (`r#self` is invalid); they
// get an underscore suffix instead.
var rustNonRawKeywords = map[string]bool{
	"self": true, "Self": true, "super": true, "crate": true, "extern": true,
	"_": true,
}

// escapeIdent makes an identifier safe to use in Rust source.
func escapeIdent(name string) string {
	if rustNonRawKeywords[name] {
		return name + "_"
	}
	if rustKeywords[name] {
		return "r#" + name
	}
	return name
}

// isNameSeparator reports whether r is a rune schemas use to separate words
// but that cannot appear in a Rust identifier. Each run of them becomes a
// single underscore.
func isNameSeparator(r rune) bool {
	return r == '-' || r == '.' || r == ' ' || r == '/' || r == ':' || r == '_'
}

// startsWord reports whether chars[i] begins a new word. chars holds only the
// letters and digits of the name; i is greater than zero.
//
// The base rule is the one every camelCase-to-snake_case converter uses, and
// the one the Pulumi Python generator's PyName implements as a state machine
// (pulumi/pkg/codegen/python/python.go:49):
//
//   - a capital after a lower-case letter or a digit opens a word, so
//     "kubeletConfigKey" splits three ways and "ipv4Address" splits two;
//   - a run of capitals is one word, and when a lower-case letter ends the run
//     the *last* capital of the run belongs to the next word instead:
//     "HTTPServer" is "http" + "server", "podCIDRSet" is "pod" + "cidr" +
//     "set";
//   - a run of capitals that reaches the end of the name stays whole, so
//     "parseJSON" is "parse" + "json";
//   - a digit never opens a word, which keeps "SHA256Hash" at "sha256" +
//     "hash" rather than splitting the number off its acronym.
//
// Two lookahead exceptions keep shapes that are common in provider schemas
// from being shredded by the run-ending rule:
//
//   - a plural acronym: an "s" that closes a run of capitals and is not itself
//     the start of a word keeps the run intact, so "podCIDRs" is "pod_cidrs"
//     and "podIPs" is "pod_ips", not "pod_cid_rs" and "pod_i_ps". Python folds
//     any trailing "s" unconditionally and has to special-case its way out of
//     the fallout; requiring the "s" to be followed by something other than a
//     lower-case letter is what keeps "openXJsonSerDe" at "open_x_json_ser_de"
//     (see pulumi/pulumi#5199).
//   - a version suffix: a single lower-case letter wedged between a run of
//     capitals and a digit belongs to the acronym, so "isIPv6Enabled" is
//     "is_ipv6_enabled" and "isNFSv3Enabled" is "is_nfsv3_enabled", not
//     "is_i_pv6_enabled" and "is_nf_sv3_enabled".
func startsWord(chars []rune, i int) bool {
	prev, cur := chars[i-1], chars[i]
	if !unicode.IsUpper(cur) {
		return false
	}
	if !unicode.IsUpper(prev) {
		return true
	}
	// Inside a run of capitals. The run only breaks where a lower-case letter
	// ends it, and then cur is the first letter of the word that follows.
	if i+1 >= len(chars) || !unicode.IsLower(chars[i+1]) {
		return false
	}
	if chars[i+1] == 's' && (i+2 >= len(chars) || !unicode.IsLower(chars[i+2])) {
		return false
	}
	if i+2 < len(chars) && unicode.IsDigit(chars[i+2]) {
		return false
	}
	return true
}

// snakeCase converts camelCase/PascalCase/kebab-case names to snake_case.
func snakeCase(name string) string {
	// Split the name into the runes that survive into the identifier, noting
	// where a word break was forced by punctuation. Runes that cannot appear
	// in a Rust identifier at all are dropped without breaking a word:
	// Kubernetes schemas carry properties like "$ref" and "$schema", whose
	// wire names are preserved separately. The .NET generator drops the same
	// leading "$" (pulumi/pkg/codegen/dotnet/gen.go:60).
	var (
		chars   []rune
		breaks  []bool
		pending bool // a separator has been seen since the last kept rune
	)
	for _, r := range name {
		switch {
		case isNameSeparator(r):
			if len(chars) > 0 {
				pending = true
			}
		case unicode.IsLetter(r) || unicode.IsDigit(r):
			chars = append(chars, r)
			breaks = append(breaks, pending)
			pending = false
		}
	}

	var b strings.Builder
	b.Grow(len(name) + 8)
	for i, r := range chars {
		switch {
		case i == 0:
			// A Rust identifier cannot start with a digit.
			if unicode.IsDigit(r) {
				b.WriteRune('_')
			}
		case breaks[i] || startsWord(chars, i):
			b.WriteRune('_')
		}
		b.WriteRune(unicode.ToLower(r))
	}
	if pending {
		b.WriteRune('_')
	}

	out := b.String()
	if out == "" {
		out = "_"
	}
	return out
}

// fieldName converts a schema property name to a Rust struct field name.
func fieldName(name string) string {
	return escapeIdent(snakeCase(name))
}

// functionName converts a schema member name to a Rust function name.
func functionName(name string) string {
	return escapeIdent(snakeCase(name))
}

// pascalCase converts a name to PascalCase for type names.
func pascalCase(name string) string {
	parts := strings.FieldsFunc(name, func(r rune) bool {
		return r == '-' || r == '.' || r == '_' || r == ' ' || r == '/' || r == ':'
	})
	var b strings.Builder
	for _, p := range parts {
		runes := []rune(p)
		if len(runes) == 0 {
			continue
		}
		b.WriteRune(unicode.ToUpper(runes[0]))
		b.WriteString(string(runes[1:]))
	}
	out := b.String()
	if out == "" {
		out = "X"
	}
	if unicode.IsDigit([]rune(out)[0]) {
		out = "X" + out
	}
	// Type-position keywords that cannot be raw identifiers.
	if out == "Self" || out == "Crate" || out == "Super" {
		out += "_"
	}
	return out
}

// crateName returns the Rust crate name for a Pulumi package name.
func crateName(pkgName string) string {
	sanitized := strings.Map(func(r rune) rune {
		switch {
		case r >= 'a' && r <= 'z', r >= '0' && r <= '9', r == '_':
			return r
		case r >= 'A' && r <= 'Z':
			return unicode.ToLower(r)
		default:
			return '_'
		}
	}, pkgName)
	return "pulumi_" + sanitized
}

// tokenMember returns the member (third) part of a Pulumi type token.
func tokenMember(token string) string {
	parts := strings.Split(token, ":")
	return parts[len(parts)-1]
}

// modIdent converts a schema module name into a Rust module identifier.
// The name "types" is reserved for the generated types module.
func modIdent(mod string) string {
	out := escapeIdent(snakeCase(mod))
	if out == "types" {
		out = "types_"
	}
	return out
}

// componentModuleName maps a component's directory to a Rust module name.
func componentModuleName(dirPath string) string {
	return escapeIdent(snakeCase(filepath.Base(dirPath)))
}

// componentReservedMembers are identifiers a generated component module
// emits itself. A component output or config folding onto one of these
// would collide with the struct field or method, so it gets a suffix.
var componentReservedMembers = map[string]bool{
	"resource": true, "pulumi_resource": true, "pulumi_deferred": true,
	"new": true, "default": true,
}

// componentMemberName maps a component config or output name to the Rust
// field and accessor name it uses.
func componentMemberName(name string) string {
	out := fieldName(name)
	if componentReservedMembers[out] {
		out += "_"
	}
	return out
}
