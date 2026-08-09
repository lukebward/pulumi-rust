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
	"strings"
	"testing"
)

// A resource with one required input among several optional ones — the shape
// that made generated code unusable when required inputs were bare types,
// because a Rust struct literal has to name every field.
const requiredInputSchema = `{
  "name": "req",
  "version": "1.0.0",
  "types": {
    "req:index:Nested": {
      "type": "object",
      "properties": { "name": { "type": "string" }, "size": { "type": "integer" } },
      "required": ["name"]
    }
  },
  "resources": {
    "req:index:Thing": {
      "inputProperties": {
        "bucket": { "type": "string" },
        "acl": { "type": "string" },
        "nested": { "$ref": "#/types/req:index:Nested" }
      },
      "requiredInputs": ["bucket"],
      "properties": { "nested": { "$ref": "#/types/req:index:Nested" } }
    }
  },
  "functions": {
    "req:index:getThing": {
      "inputs": {
        "properties": { "id": { "type": "string" }, "hint": { "type": "string" } },
        "required": ["id"]
      },
      "outputs": { "properties": { "arn": { "type": "string" } }, "required": ["arn"] }
    }
  }
}`

func TestEveryArgsStructDerivesDefault(t *testing.T) {
	lib := generate(t, requiredInputSchema)

	// Resource args, function args and nested object args alike: a caller
	// names what it sets and elides the rest.
	for _, want := range []string{
		"#[derive(Clone, Debug, Default)]\npub struct ThingArgs {",
		"#[derive(Clone, Debug, Default)]\n    pub struct NestedArgs {",
		"#[derive(Clone, Debug, Default)]\npub struct GetThingArgs {",
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%q", want)
		}
	}
}

func TestRequiredInputsAreStillOptionFields(t *testing.T) {
	lib := generate(t, requiredInputSchema)

	// `bucket` is required by the schema, but declaring it as a bare
	// `Output<String>` would cost `Default` for the whole struct and force
	// every call site to name all three fields.
	for _, want := range []string{
		"pub bucket: Option<pulumi::Output<std::string::String>>,",
		"pub acl: Option<pulumi::Output<std::string::String>>,",
		"pub nested: Option<crate::types::NestedArgs>,",
		"pub id: Option<pulumi::Output<std::string::String>>,",
		"pub name: Option<pulumi::Output<std::string::String>>,",
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%q", want)
		}
	}

	if strings.Contains(lib, "#[derive(Clone, Debug)]\npub struct ThingArgs") {
		t.Error("ThingArgs lost its Default derive")
	}
}

func TestUnsetInputsAreNotSentToTheEngine(t *testing.T) {
	lib := generate(t, requiredInputSchema)

	// Every field, required or not, is pushed only when set. Sending a
	// missing input as null would be a real difference: providers treat an
	// explicit null as "unset this", not "leave it alone".
	for _, want := range []string{
		`if let Some(v) = self.bucket {`,
		`inputs.push(("bucket".to_string(), v.cast()));`,
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%q", want)
		}
	}
	if strings.Contains(lib, "let v = self.bucket;") {
		t.Error("bucket is still pushed unconditionally")
	}
}

// Output structs are read, not constructed, so they keep tracking the schema:
// a required output stays a bare type and an optional one stays an Option.
// Loosening them would push an Option onto every caller reading a value that
// is always present.
func TestOutputStructsStillFollowTheSchema(t *testing.T) {
	lib := generate(t, requiredInputSchema)
	if !strings.Contains(lib, "pub struct Nested {\n        pub name: std::string::String,") {
		t.Error("a required output property should not be optional")
	}
	if !strings.Contains(lib, "pub size: Option<i32>,") {
		t.Error("an optional output property should stay optional")
	}
}

// Because required-ness is no longer visible in the Rust types, the generated
// resource carries the schema's required wire names so the runtime can report
// a forgotten input by name instead of leaving it to the provider.
func TestRequiredWireNamesReachTheRuntime(t *testing.T) {
	lib := generate(t, requiredInputSchema)
	if !strings.Contains(lib, `required: &["bucket"],`) {
		t.Error("the resource does not declare its required inputs")
	}
	// The provider resource has no required inputs, and must still compile.
	if !strings.Contains(lib, "required: &[],") {
		t.Error("a resource with no required inputs should declare an empty slice")
	}
	// Wire names, not Rust field names: that is what the schema, the docs and
	// every other language show.
	if strings.Contains(lib, `required: &["r#type"]`) {
		t.Error("required names must be wire names, not escaped Rust identifiers")
	}
}
