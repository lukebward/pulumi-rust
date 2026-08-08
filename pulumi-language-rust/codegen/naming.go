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

// snakeCase converts camelCase/PascalCase/kebab-case names to snake_case.
func snakeCase(name string) string {
	var b strings.Builder
	prevLower := false
	prevUnderscore := true // suppress leading underscore
	for _, r := range name {
		switch {
		case r == '-' || r == '.' || r == ' ' || r == '/' || r == ':' || r == '_':
			if !prevUnderscore {
				b.WriteRune('_')
				prevUnderscore = true
			}
			prevLower = false
			continue
		case unicode.IsUpper(r):
			if prevLower && !prevUnderscore {
				b.WriteRune('_')
			}
			b.WriteRune(unicode.ToLower(r))
			prevLower = false
			prevUnderscore = false
		case unicode.IsDigit(r):
			if b.Len() == 0 {
				b.WriteRune('_')
			}
			b.WriteRune(r)
			prevLower = false
			prevUnderscore = false
		default:
			b.WriteRune(r)
			prevLower = true
			prevUnderscore = false
		}
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
