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

import (
	"regexp"
	"strings"
	"testing"
)

// The two collision shapes a token can produce. A Rust type name keeps only
// a token's module and member, so tokens that agree on both — however they
// differ elsewhere — used to be emitted as two structs of the same name in
// the same module.
//
// The first is aws@7.41.0's, reproduced token for token under a different
// package name: `getPrincipalPolicySimulation` is a function, so the binder
// mints its result type as the function's member plus "Result", which is
// exactly the member of a type the schema already declares.
//
// The second is two declared types in sibling submodules of one module:
// `ec2/instance` and `ec2/volume` both collapse to module `ec2` under this
// moduleFormat, and both declare a `Filter`.
const collidingNamesSchema = `{
  "name": "coll",
  "version": "1.0.0",
  "meta": { "moduleFormat": "(.*)(?:/[^/]*)" },
  "types": {
    "coll:iam/getPrincipalPolicySimulationResult:getPrincipalPolicySimulationResult": {
      "type": "object",
      "properties": { "allowed": { "type": "boolean" } },
      "required": ["allowed"]
    },
    "coll:ec2/instance:Filter": {
      "type": "object",
      "properties": { "name": { "type": "string" } },
      "required": ["name"]
    },
    "coll:ec2/volume:Filter": {
      "type": "object",
      "properties": { "size": { "type": "integer" } },
      "required": ["size"]
    }
  },
  "resources": {
    "coll:ec2/instance:Instance": {
      "inputProperties": {
        "filters": {
          "type": "array",
          "items": { "$ref": "#/types/coll:ec2/instance:Filter" }
        },
        "volumeFilters": {
          "type": "array",
          "items": { "$ref": "#/types/coll:ec2/volume:Filter" }
        }
      },
      "properties": {
        "filters": {
          "type": "array",
          "items": { "$ref": "#/types/coll:ec2/instance:Filter" }
        },
        "volumeFilters": {
          "type": "array",
          "items": { "$ref": "#/types/coll:ec2/volume:Filter" }
        }
      }
    }
  },
  "functions": {
    "coll:iam/getPrincipalPolicySimulation:getPrincipalPolicySimulation": {
      "inputs": {
        "properties": { "policySourceArn": { "type": "string" } },
        "required": ["policySourceArn"]
      },
      "outputs": {
        "properties": {
          "results": {
            "type": "array",
            "items": {
              "$ref": "#/types/coll:iam/getPrincipalPolicySimulationResult:getPrincipalPolicySimulationResult"
            }
          }
        },
        "required": ["results"]
      }
    }
  }
}`

var declRe = regexp.MustCompile(`(?m)^\s*pub (?:struct|enum) ([A-Za-z0-9_]+)\b`)

// declarationsIn reports how many times each type name is declared inside
// `pub mod types`, which is where every object type in a generated crate
// lands and therefore where two tokens sharing a name become two structs
// sharing a name.
func declarationsIn(t *testing.T, lib, module string) map[string]int {
	t.Helper()
	head := "pub mod " + module + " {\n"
	i := strings.Index(lib, head)
	if i < 0 {
		t.Fatalf("generated lib.rs has no %q module", module)
	}
	body := lib[i+len(head):]
	// The module runs to the first line-initial `}`; everything inside it
	// is indented.
	if j := strings.Index(body, "\n}"); j >= 0 {
		body = body[:j]
	}
	counts := map[string]int{}
	for _, m := range declRe.FindAllStringSubmatch(body, -1) {
		counts[m[1]]++
	}
	if len(counts) == 0 {
		t.Fatalf("found no declarations in the %q module", module)
	}
	return counts
}

func TestCollidingTokensGetDistinctTypeNames(t *testing.T) {
	lib := generate(t, collidingNamesSchema)

	for name, n := range declarationsIn(t, lib, "types") {
		if n > 1 {
			t.Errorf("`%s` is declared %d times in the types module", name, n)
		}
	}
}

// Between a type the schema declares and a function result the binder
// synthesizes, the declared one keeps the undecorated name: it can be
// referenced from anywhere in the schema, while a function result is only
// ever named by its own invoke.
func TestFunctionResultsYieldToDeclaredTypes(t *testing.T) {
	lib := generate(t, collidingNamesSchema)

	for _, want := range []string{
		// The declared type, still under its derived name, and still what
		// the function result's `results` field is a Vec of.
		"pub struct IamGetPrincipalPolicySimulationResult {",
		"pub results: std::vec::Vec<crate::types::IamGetPrincipalPolicySimulationResult>,",
		// The synthesized result, renamed, and returned by the invoke.
		"pub struct IamGetPrincipalPolicySimulationResult2 {",
		"-> pulumi::Output<crate::types::IamGetPrincipalPolicySimulationResult2>",
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%q", want)
		}
	}
}

// Two declared types collide on equal footing, so the tie breaks on the
// token — deterministically, and without either one taking a suffix it does
// not need.
func TestDeclaredCollisionsAreBrokenDeterministically(t *testing.T) {
	first := generate(t, collidingNamesSchema)
	if !strings.Contains(first, "pub struct Ec2Filter {") ||
		!strings.Contains(first, "pub struct Ec2Filter2 {") {
		t.Fatalf("expected `Ec2Filter` and `Ec2Filter2`; got neither")
	}
	// `coll:ec2/instance:Filter` sorts before `coll:ec2/volume:Filter`, so
	// the instance filter — the one with `name` — is the unsuffixed one.
	nameField := strings.Index(first, "pub struct Ec2Filter {")
	sizeField := strings.Index(first, "pub struct Ec2Filter2 {")
	if !strings.Contains(first[nameField:sizeField], "pub name:") {
		t.Errorf("`Ec2Filter` should be the instance filter, which has `name`")
	}
	// Nothing about the assignment depends on map iteration order.
	for i := 0; i < 5; i++ {
		if got := generate(t, collidingNamesSchema); got != first {
			t.Fatalf("generation is not deterministic")
		}
	}
}

// A suffixed name must not collide in turn: `Name` and `NameArgs` are
// reserved together, so a token that would derive `Ec2Filter2` on its own
// still gets a name of its own.
func TestSuffixedNamesDoNotCollideInTurn(t *testing.T) {
	lib := generate(t, strings.Replace(
		collidingNamesSchema,
		`"coll:ec2/volume:Filter": {`,
		`"coll:ec2/snapshot:Filter2": {
      "type": "object",
      "properties": { "id": { "type": "string" } },
      "required": ["id"]
    },
    "coll:ec2/volume:Filter": {`, 1))

	for name, n := range declarationsIn(t, lib, "types") {
		if n > 1 {
			t.Errorf("`%s` is declared %d times in the types module", name, n)
		}
	}
}
