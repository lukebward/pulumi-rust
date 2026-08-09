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
	"encoding/json"
	"strings"
	"testing"

	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
)

// The provider-config pattern every cloud provider uses: an optional input
// whose real default lives in the operator's environment. Without it an
// unset input sends nothing and the provider fails with "missing required
// configuration" over a value the schema promised was always there.
const envDefaultSchema = `{
  "name": "envdefault",
  "version": "1.0.0",
  "resources": {
    "envdefault:index:Thing": {
      "inputProperties": {
        "region": {
          "type": "string",
          "default": "us-west-2",
          "defaultInfo": { "environment": ["AWS_REGION", "AWS_DEFAULT_REGION"] }
        },
        "insecure": {
          "type": "boolean",
          "defaultInfo": { "environment": ["THING_INSECURE"] }
        },
        "retries": {
          "type": "integer",
          "defaultInfo": { "environment": ["THING_RETRIES"] }
        },
        "ratio": {
          "type": "number",
          "defaultInfo": { "environment": ["THING_RATIO"] }
        },
        "tags": {
          "type": "array",
          "items": { "type": "string" },
          "defaultInfo": { "environment": ["THING_TAGS"] }
        }
      }
    }
  }
}`

func TestEnvironmentDefaults(t *testing.T) {
	lib := generate(t, envDefaultSchema)

	for _, want := range []string{
		// Every named variable is probed, in the order the schema lists them.
		`} else if let Some(v) = crate::internal::env_string(&["AWS_REGION", "AWS_DEFAULT_REGION"]) {`,
		`inputs.push(("region".to_string(), pulumi::Output::from_value(v)));`,
		// The static default is still there, behind the environment.
		`inputs.push(("region".to_string(), ` +
			`pulumi::Output::from_value(pulumi::PropertyValue::String("us-west-2".to_string()))));`,
		// The value is decoded as the type the schema declares, not as a
		// string that would fail the provider's own validation.
		`} else if let Some(v) = crate::internal::env_bool(&["THING_INSECURE"]) {`,
		`} else if let Some(v) = crate::internal::env_int(&["THING_RETRIES"]) {`,
		`} else if let Some(v) = crate::internal::env_number(&["THING_RATIO"]) {`,
		// The helpers the generated code calls.
		"pub mod internal {",
		"pub fn env_string(names: &[&str]) -> Option<pulumi::PropertyValue> {",
	} {
		if !strings.Contains(lib, want) {
			t.Errorf("generated lib.rs is missing:\n\t%s", want)
		}
	}

	// An environment variable holds a string; there is nothing sensible to
	// decode one into for a collection, so that property is left alone
	// rather than given a fallback that cannot work.
	if strings.Contains(lib, "THING_TAGS") {
		t.Error("an array-typed property should get no environment fallback")
	}
}

// The environment wins over the static default: an operator who exports
// AWS_REGION means it to beat the value baked into the schema.
func TestEnvironmentDefaultPrecedesStaticDefault(t *testing.T) {
	lib := generate(t, envDefaultSchema)

	envAt := strings.Index(lib, `crate::internal::env_string(&["AWS_REGION"`)
	staticAt := strings.Index(lib, `pulumi::PropertyValue::String("us-west-2".to_string())`)
	if envAt < 0 || staticAt < 0 {
		t.Fatalf("expected both branches, got env=%d static=%d", envAt, staticAt)
	}
	if envAt > staticAt {
		t.Error("the environment branch should be tried before the static default")
	}
}

// The helper module costs nothing to a crate that never reads the
// environment, and every existing SDK snapshot depends on it not appearing.
func TestNoEnvHelpersWithoutEnvironmentDefaults(t *testing.T) {
	lib := generate(t, `{
	  "name": "plaindefault",
	  "version": "1.0.0",
	  "resources": {
	    "plaindefault:index:Thing": {
	      "inputProperties": { "region": { "type": "string", "default": "us-west-2" } }
	    }
	  }
	}`)

	if strings.Contains(lib, "pub mod internal") || strings.Contains(lib, "std::env") {
		t.Error("a schema with no environment defaults should emit no environment helpers")
	}
	want := `        if let Some(v) = self.region {
            inputs.push(("region".to_string(), v.cast()));
        } else {
            inputs.push(("region".to_string(), ` +
		`pulumi::Output::from_value(pulumi::PropertyValue::String("us-west-2".to_string()))));
        }
`
	if !strings.Contains(lib, want) {
		t.Errorf("a plain static default should be emitted unchanged; got:\n%s", lib)
	}
}

// A default that reaches the renderer in a shape it does not know must be a
// generation error. The caller reads "no expression" as "no default", so
// returning an empty string quietly drops a value the schema declared and
// leaves the provider to fail at runtime instead.
func TestPlainConstValue(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name  string
		value any
		want  string
	}{
		{"bool", true, "pulumi::PropertyValue::Bool(true)"},
		{"int32", int32(1), "pulumi::PropertyValue::Number(1.0)"},
		{"int", 42, "pulumi::PropertyValue::Number(42.0)"},
		{"int64", int64(42), "pulumi::PropertyValue::Number(42.0)"},
		{"float", 0.5, "pulumi::PropertyValue::Number(0.5)"},
		{"whole float", float64(2), "pulumi::PropertyValue::Number(2.0)"},
		{"string", "hi", `pulumi::PropertyValue::String("hi".to_string())`},
		// A schema decoded with json.Decoder.UseNumber keeps its numbers as
		// text; the old renderer dropped every one of them.
		{"json.Number integer", json.Number("3"), "pulumi::PropertyValue::Number(3.0)"},
		{"json.Number float", json.Number("3.5"), "pulumi::PropertyValue::Number(3.5)"},
		{
			"slice",
			[]any{"a", 1.0},
			`pulumi::PropertyValue::Array(vec![pulumi::PropertyValue::String("a".to_string()), ` +
				`pulumi::PropertyValue::Number(1.0)])`,
		},
		{
			"map",
			map[string]any{"b": 2.0, "a": "x"},
			`pulumi::PropertyValue::Object(std::collections::BTreeMap::from([` +
				`("a".to_string(), pulumi::PropertyValue::String("x".to_string())), ` +
				`("b".to_string(), pulumi::PropertyValue::Number(2.0))]))`,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()
			got, err := plainConstValue(tt.value)
			if err != nil {
				t.Fatalf("plainConstValue(%#v): %v", tt.value, err)
			}
			if got != tt.want {
				t.Errorf("plainConstValue(%#v) =\n\t%s\nwant\n\t%s", tt.value, got, tt.want)
			}
		})
	}

	t.Run("an unrenderable value is an error, not silence", func(t *testing.T) {
		t.Parallel()
		got, err := plainConstValue(struct{ A int }{1})
		if err == nil {
			t.Fatalf("expected an error, got %q", got)
		}
	})
}

// And the error has to reach the caller rather than being swallowed into a
// crate that is missing a default.
func TestUnrenderableDefaultFailsGeneration(t *testing.T) {
	t.Parallel()

	var spec schema.PackageSpec
	if err := json.Unmarshal([]byte(`{
	  "name": "weird",
	  "version": "1.0.0",
	  "resources": {
	    "weird:index:Thing": {
	      "inputProperties": { "region": { "type": "string", "default": "us-west-2" } }
	    }
	  }
	}`), &spec); err != nil {
		t.Fatalf("unmarshal schema: %v", err)
	}
	pkg, diags, err := schema.BindSpec(spec, noLoader{}, schema.ValidationOptions{AllowDanglingReferences: true})
	if err != nil || diags.HasErrors() {
		t.Fatalf("bind schema: %v %v", err, diags)
	}

	// Binding will not produce a default of this shape, so plant one: the
	// point is that nothing downstream of the binder can drop a default on
	// the floor without saying so.
	for _, r := range pkg.Resources {
		for _, p := range r.InputProperties {
			p.DefaultValue.Value = struct{ Unrenderable bool }{true}
		}
	}

	files, err := GeneratePackage("test", pkg, nil, nil)
	if err == nil {
		t.Fatalf("expected generation to fail; got lib.rs:\n%s", files["src/lib.rs"])
	}
	if !strings.Contains(err.Error(), "region") {
		t.Errorf("the error should name the offending input; got: %v", err)
	}
}
