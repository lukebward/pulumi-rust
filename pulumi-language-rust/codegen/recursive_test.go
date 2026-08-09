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
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"

	"github.com/blang/semver"
	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
)

// noLoader stands in for the plugin loader the binder wants. These schemas
// are self-contained, so nothing should ever be loaded; saying so loudly
// beats silently binding against whatever is installed on the machine.
type noLoader struct{}

func (noLoader) LoadPackage(pkg string, _ *semver.Version) (*schema.Package, error) {
	return nil, errors.New("unexpected package load: " + pkg)
}

func (noLoader) LoadPackageV2(
	_ context.Context, descriptor *schema.PackageDescriptor,
) (*schema.Package, error) {
	return nil, errors.New("unexpected package load: " + descriptor.Name)
}

// generate runs GeneratePackage over a schema spec and returns src/lib.rs.
func generate(t *testing.T, spec string) string {
	t.Helper()
	var pkgSpec schema.PackageSpec
	if err := json.Unmarshal([]byte(spec), &pkgSpec); err != nil {
		t.Fatalf("unmarshal schema: %v", err)
	}
	pkg, diags, err := schema.BindSpec(pkgSpec, noLoader{}, schema.ValidationOptions{
		AllowDanglingReferences: true,
	})
	if err != nil {
		t.Fatalf("bind schema: %v", err)
	}
	if diags.HasErrors() {
		t.Fatalf("bind schema: %v", diags)
	}
	files, err := GeneratePackage("test", pkg, nil, nil)
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	return string(files["src/lib.rs"])
}

// A type whose own property is of its own type — the shape Kubernetes'
// `JSONSchemaProps.not` has. Emitted bare it is an infinitely sized Rust
// struct, so the field has to be boxed.
const selfReferentialSchema = `{
  "name": "recur",
  "version": "1.0.0",
  "types": {
    "recur:index:Node": {
      "type": "object",
      "properties": {
        "value": { "type": "string" },
        "not": { "$ref": "#/types/recur:index:Node" },
        "children": { "type": "array", "items": { "$ref": "#/types/recur:index:Node" } },
        "byName": { "type": "object", "additionalProperties": { "$ref": "#/types/recur:index:Node" } }
      }
    }
  },
  "resources": {
    "recur:index:Tree": {
      "inputProperties": { "root": { "$ref": "#/types/recur:index:Node" } },
      "properties": { "root": { "$ref": "#/types/recur:index:Node" } }
    }
  }
}`

func TestSelfReferentialTypeIsBoxed(t *testing.T) {
	lib := generate(t, selfReferentialSchema)

	for _, want := range []string{
		// The cycle-closing field, on both the args and the output struct.
		"pub not: Option<std::boxed::Box<crate::types::NodeArgs>>,",
		"pub not: Option<std::boxed::Box<crate::types::Node>>,",
		// Moving out of the box on the way to the wire.
		"inputs.push((\"not\".to_string(), (*v).into_output()));",
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%s", want)
		}
	}

	// A collection is already a separate allocation, so a type reached
	// through one is not part of its container's size and must not be
	// boxed — boxing it would be a gratuitous indirection.
	for _, unwanted := range []string{
		"pub children: Option<std::vec::Vec<std::boxed::Box<",
		"pub by_name: Option<std::collections::BTreeMap<std::string::String, std::boxed::Box<",
	} {
		if strings.Contains(lib, unwanted) {
			t.Errorf("generated lib.rs boxes a field behind a collection:\n\t%s", unwanted)
		}
	}

	// The resource's own structs are not part of any cycle: no object type
	// refers back to them, so their fields stay unboxed.
	if !strings.Contains(lib, "pub root: Option<crate::types::NodeArgs>,") {
		t.Error("resource args struct should not box its object field")
	}
}

// Two types that refer to each other close a cycle just as a self-reference
// does, and both directions get boxed.
const mutuallyRecursiveSchema = `{
  "name": "recur",
  "version": "1.0.0",
  "types": {
    "recur:index:Even": {
      "type": "object",
      "properties": { "next": { "$ref": "#/types/recur:index:Odd" } }
    },
    "recur:index:Odd": {
      "type": "object",
      "properties": { "next": { "$ref": "#/types/recur:index:Even" } }
    },
    "recur:index:Leaf": {
      "type": "object",
      "properties": { "even": { "$ref": "#/types/recur:index:Even" } }
    }
  },
  "resources": {
    "recur:index:Chain": {
      "inputProperties": { "leaf": { "$ref": "#/types/recur:index:Leaf" } }
    }
  }
}`

func TestMutuallyRecursiveTypesAreBoxed(t *testing.T) {
	lib := generate(t, mutuallyRecursiveSchema)

	for _, want := range []string{
		"pub struct EvenArgs {\n        pub next: Option<std::boxed::Box<crate::types::OddArgs>>,",
		"pub struct OddArgs {\n        pub next: Option<std::boxed::Box<crate::types::EvenArgs>>,",
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%s", want)
		}
	}

	// `Leaf` points into the cycle but is not part of it — reaching `Even`
	// from `Leaf` never comes back — so its field is left alone.
	if !strings.Contains(lib, "pub struct LeafArgs {\n        pub even: Option<crate::types::EvenArgs>,") {
		t.Error("a type that only points into a cycle should not be boxed")
	}
}

// The common case: nothing recursive, nothing boxed. This is what keeps the
// change invisible to every existing snapshot.
const acyclicSchema = `{
  "name": "flat",
  "version": "1.0.0",
  "types": {
    "flat:index:Inner": {
      "type": "object",
      "properties": { "value": { "type": "string" } }
    },
    "flat:index:Outer": {
      "type": "object",
      "properties": { "inner": { "$ref": "#/types/flat:index:Inner" } }
    }
  },
  "resources": {
    "flat:index:Thing": {
      "inputProperties": { "outer": { "$ref": "#/types/flat:index:Outer" } },
      "properties": { "outer": { "$ref": "#/types/flat:index:Outer" } }
    }
  }
}`

func TestAcyclicTypesAreNotBoxed(t *testing.T) {
	lib := generate(t, acyclicSchema)
	if strings.Contains(lib, "std::boxed::Box") {
		t.Error("an acyclic schema should generate no boxes")
	}
}
