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
	"fmt"
	"strings"
	"testing"

	"github.com/blang/semver"
	"github.com/hashicorp/hcl/v2"
	"github.com/pulumi/pulumi/pkg/v3/codegen/hcl2/model"
	"github.com/pulumi/pulumi/pkg/v3/codegen/hcl2/syntax"
	"github.com/pulumi/pulumi/pkg/v3/codegen/pcl"
	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
)

// specLoader serves a fixed set of in-memory schemas to the PCL binder, so a
// program test never depends on what happens to be installed on the machine.
type specLoader struct {
	pkgs map[string]*schema.Package
}

func newSpecLoader(t *testing.T, specs ...string) *specLoader {
	t.Helper()
	l := &specLoader{pkgs: map[string]*schema.Package{}}
	for _, spec := range specs {
		var pkgSpec schema.PackageSpec
		if err := json.Unmarshal([]byte(spec), &pkgSpec); err != nil {
			t.Fatalf("unmarshal schema: %v", err)
		}
		pkg, diags, err := schema.BindSpec(pkgSpec, l, schema.ValidationOptions{
			AllowDanglingReferences: true,
		})
		if err != nil {
			t.Fatalf("bind schema: %v", err)
		}
		if diags.HasErrors() {
			t.Fatalf("bind schema: %v", diags)
		}
		l.pkgs[pkg.Name] = pkg
	}
	return l
}

func (l *specLoader) LoadPackage(name string, _ *semver.Version) (*schema.Package, error) {
	if pkg, ok := l.pkgs[name]; ok {
		return pkg, nil
	}
	return nil, fmt.Errorf("unexpected package load: %s", name)
}

func (l *specLoader) LoadPackageV2(
	_ context.Context, descriptor *schema.PackageDescriptor,
) (*schema.Package, error) {
	return l.LoadPackage(descriptor.Name, descriptor.Version)
}

// bindProgram binds one PCL source file against the given schemas.
func bindProgram(t *testing.T, source string, specs ...string) *pcl.Program {
	t.Helper()
	parser := syntax.NewParser()
	if err := parser.ParseFile(strings.NewReader(source), "main.pp"); err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parser.Diagnostics.HasErrors() {
		t.Fatalf("parse: %v", parser.Diagnostics)
	}
	program, diags, err := pcl.BindProgram(parser.Files, newSpecLoader(t, specs...),
		pcl.AllowMissingProperties, pcl.AllowMissingVariables, pcl.SkipResourceTypechecking)
	if err != nil {
		t.Fatalf("bind program: %v", err)
	}
	if diags.HasErrors() {
		t.Fatalf("bind program: %v", diags)
	}
	return program
}

// genProgram generates main.rs for a PCL source, failing on any diagnostic.
func genProgram(t *testing.T, source string, specs ...string) string {
	t.Helper()
	main, diags := genProgramDiags(t, source, specs...)
	if diags.HasErrors() {
		t.Fatalf("generate: %v", diags)
	}
	return main
}

// genProgramDiags generates main.rs and returns the generator's diagnostics.
func genProgramDiags(t *testing.T, source string, specs ...string) (string, hcl.Diagnostics) {
	t.Helper()
	files, diags, err := GenerateProgram(bindProgram(t, source, specs...))
	if err != nil {
		t.Fatalf("generate: %v", err)
	}
	return string(files["src/main.rs"]), diags
}

// A resource whose outputs exercise the ways a wire name and a Rust accessor
// can disagree: two names that fold to the same snake_case identifier, and
// names that land on methods the generated resource struct emits itself.
const accessorSchema = `{
  "name": "acc",
  "version": "1.0.0",
  "resources": {
    "acc:index:Thing": {
      "inputProperties": { "value": { "type": "string" } },
      "properties": {
        "myValue": { "type": "string" },
        "my_value": { "type": "string" },
        "id": { "type": "string" },
        "new": { "type": "string" },
        "plain": { "type": "string" }
      },
      "stateInputs": { "properties": { "value": { "type": "string" } } }
    }
  }
}`

// The program generator addresses a resource's outputs through the accessors
// the SDK generator actually emitted. Reading them off a fresh snake_case of
// the wire name silently swaps two properties that fold together, and does
// not compile at all for a name the resource struct already uses.
func TestOutputAccessorsAgreeWithTheSdk(t *testing.T) {
	main := genProgram(t, `
resource "t" "acc:index:Thing" { value = "x" }
output "a" { value = t.myValue }
output "b" { value = t.my_value }
output "c" { value = t.id }
output "d" { value = t.new }
output "e" { value = t.plain }
`, accessorSchema)

	// The names the SDK generator emits for the same schema, so the two can
	// only drift together.
	lib := generate(t, accessorSchema)
	for _, tt := range []struct{ pcl, accessor string }{
		{"a", "my_value"},
		{"b", "my_value_"},
		{"c", "id_"},
		{"d", "new_"},
		{"e", "plain"},
	} {
		if !strings.Contains(lib, "pub fn "+tt.accessor+"(&self)") {
			t.Errorf("the SDK does not emit %s(); this test's expectations are stale", tt.accessor)
		}
		want := fmt.Sprintf("ctx.export(%q, t.%s().cast", tt.pcl, tt.accessor)
		if !strings.Contains(main, want) {
			t.Errorf("output %q should read t.%s():\n%s", tt.pcl, tt.accessor, main)
		}
	}
	// The engine's own id() would compile here and quietly return a different
	// value from the schema's `id` property, which is the one PCL binds.
	if strings.Contains(main, "t.id().cast") {
		t.Error("a schema property named id must not resolve to the engine id")
	}
}

// A resource with no output of that name still reaches the engine's urn and
// id accessors, which is what a bare PCL `res.id` means.
func TestEngineUrnAndIdStillResolve(t *testing.T) {
	main := genProgram(t, `
resource "t" "acc:index:Thing" { value = "x" }
output "u" { value = t.urn }
`, accessorSchema)
	if !strings.Contains(main, "t.urn().cast") {
		t.Errorf("urn should resolve to the engine accessor:\n%s", main)
	}
}

const readSchema = `{
  "name": "rd",
  "version": "2.0.0",
  "resources": {
    "rd:index:Thing": {
      "inputProperties": { "value": { "type": "string" } },
      "properties": { "value": { "type": "string" } },
      "stateInputs": { "properties": { "value": { "type": "string" } } }
    }
  }
}`

// A read binds a program variable like every other node, so it has to be
// declared rather than merely named: a move closure that captures it must
// clone it, or the generated program fails to compile on the next use.
func TestReadResourceIsCapturedByClosures(t *testing.T) {
	main := genProgram(t, `
read "r" "rd:index:Thing" { id = "abc" }
inLoop = [for x in [1, 2]: r.value]
after = r.value
`, readSchema)
	if !strings.Contains(main, "let r = r.clone();") {
		t.Errorf("a read captured by a move closure must be cloned:\n%s", main)
	}
}

// Distinct PCL names that fold to the same snake_case identifier have to be
// kept apart, and a read participates in that uniquing like anything else.
func TestReadResourceTakesPartInUniquing(t *testing.T) {
	main := genProgram(t, `
read "myRead" "rd:index:Thing" { id = "abc" }
my_read = "shadow"
out = myRead.value
`, readSchema)
	if strings.Contains(main, "let my_read = pulumi::pv::string(\"shadow\");\nlet out = my_read.output") {
		t.Error("the local shadowed the read resource")
	}
	if !strings.Contains(main, `let out = my_read.output("value")`) &&
		!strings.Contains(main, `let out = my_read_.output("value")`) {
		t.Errorf("expected the read's own binding to be referenced:\n%s", main)
	}
	// Whatever identifier the local ends up with, the read's reference must
	// resolve to the read's binding and not to the local's.
	readLine := ""
	localLine := ""
	for _, line := range strings.Split(main, "\n") {
		if strings.Contains(line, "ctx.read_resource(") {
			readLine = line
		}
		if strings.Contains(line, `pulumi::pv::string("shadow")`) {
			localLine = line
		}
	}
	readIdent := letBinding(readLine)
	localIdent := letBinding(localLine)
	if readIdent == "" || localIdent == "" {
		t.Fatalf("could not find both bindings:\n%s", main)
	}
	if readIdent == localIdent {
		t.Errorf("the read and the local share the identifier %q:\n%s", readIdent, main)
	}
	if !strings.Contains(main, "let out = "+readIdent+".output(\"value\")") {
		t.Errorf("the reference should resolve to %q:\n%s", readIdent, main)
	}
}

// letBinding pulls the identifier out of a generated `let <ident> = ...` line.
func letBinding(line string) string {
	line = strings.TrimSpace(line)
	rest, ok := strings.CutPrefix(line, "let ")
	if !ok {
		return ""
	}
	ident, _, ok := strings.Cut(rest, " = ")
	if !ok {
		return ""
	}
	return ident
}

// A read's options were dropped on the floor: the read ran against the
// default provider, parented to the stack root, with nothing said about it.
func TestReadResourceOptionsAreWiredThrough(t *testing.T) {
	main := genProgram(t, `
resource "prov" "pulumi:providers:rd" { }
resource "parent" "rd:index:Thing" { value = "p" }
read "r" "rd:index:Thing" {
  id = "abc"
  options {
    provider = prov
    parent = parent
    dependsOn = [parent]
    additionalSecretOutputs = [value]
  }
}
`, readSchema)
	for _, want := range []string{
		"provider: Some(prov.pulumi_resource().clone())",
		"parent: Some(parent.pulumi_resource().clone())",
		"depends_on: vec![parent.pulumi_resource().clone()]",
		`additional_secret_outputs: vec!["value".to_string()]`,
	} {
		if !strings.Contains(main, want) {
			t.Errorf("the read's options are missing %s:\n%s", want, main)
		}
	}
}

// A version option names the plugin to read through, which reaches
// read_resource as its own argument rather than through the options.
func TestReadResourceVersionOverridesTheSchema(t *testing.T) {
	main := genProgram(t, `
read "r" "rd:index:Thing" {
  id = "abc"
  options { version = "3.1.0" }
}
`, readSchema)
	if !strings.Contains(main, `vec![], "3.1.0",`) {
		t.Errorf("the version option should reach read_resource:\n%s", main)
	}
}

// A read is not a managed resource, so the lifecycle options have nothing to
// act on. Saying so beats rendering options the runtime would ignore.
func TestUnsupportedReadOptionsAreReported(t *testing.T) {
	for _, tt := range []struct{ option, source string }{
		{"protect", "protect = true"},
		{"retainOnDelete", "retainOnDelete = true"},
		{"deleteBeforeReplace", "deleteBeforeReplace = true"},
		{"ignoreChanges", "ignoreChanges = [value]"},
		{"import", "import = \"some-id\""},
		{"aliases", "aliases = [\"urn:pulumi:a::b::c::d\"]"},
		{"pluginDownloadURL", "pluginDownloadURL = \"https://example.com\""},
	} {
		t.Run(tt.option, func(t *testing.T) {
			_, diags := genProgramDiags(t, fmt.Sprintf(`
read "r" "rd:index:Thing" {
  id = "abc"
  options { %s }
}
`, tt.source), readSchema)
			want := fmt.Sprintf("resource option %q is not supported on a read", tt.option)
			for _, d := range diags {
				if d.Summary == want {
					return
				}
			}
			t.Errorf("expected a diagnostic for %s, got %v", tt.option, diags)
		})
	}
}

const unionSchema = `{
  "name": "uni",
  "version": "1.0.0",
  "resources": {
    "uni:index:Thing": {
      "inputProperties": {
        "either": { "oneOf": [{ "type": "string" }, { "type": "number" }] },
        "maybe": { "type": "string" },
        "count": { "type": "integer" }
      },
      "properties": {}
    }
  }
}`

// A union destination names several representations, not one to coerce to.
// Picking the numeric arm turned the string literal "1e5" into 100000.0 and
// handed the provider a number where the program wrote a string.
func TestUnionDestinationsSuppressCoercion(t *testing.T) {
	main := genProgram(t, `
resource "t" "uni:index:Thing" { either = "1e5" }
`, unionSchema)
	if strings.Contains(main, "to_number") || strings.Contains(main, "to_int") {
		t.Errorf("a union destination must not coerce:\n%s", main)
	}
	if !strings.Contains(main, `either: Some(pulumi::pv::string("1e5").cast())`) {
		t.Errorf("the string should reach the provider unchanged:\n%s", main)
	}
}

// An optional type is a union with none, and still names one representation.
// Suppressing coercion for every union would take those conversions with it.
func TestOptionalDestinationsStillCoerce(t *testing.T) {
	main := genProgram(t, `
config "n" "number" { }
resource "t" "uni:index:Thing" {
  maybe = n
  count = "7"
}
`, unionSchema)
	if !strings.Contains(main, "pulumi::ops::to_string(n.clone())") {
		t.Errorf("an optional string destination should still coerce:\n%s", main)
	}
	if !strings.Contains(main, "count: Some(pulumi::ops::to_int(") {
		t.Errorf("an optional int destination should still coerce:\n%s", main)
	}
}

func TestConversionKind(t *testing.T) {
	str, num := model.StringType, model.NumberType
	for _, tt := range []struct {
		name string
		t    model.Type
		want string
	}{
		{"string", str, "string"},
		{"number", num, "number"},
		{"int", model.IntType, "int"},
		{"bool", model.BoolType, "bool"},
		{"optional string", model.NewOptionalType(str), "string"},
		{"optional number", model.NewOptionalType(num), "number"},
		{"output of string", model.NewOutputType(str), "string"},
		{"string or number", model.NewUnionType(str, num), ""},
		{"optional string or number", model.NewOptionalType(model.NewUnionType(str, num)), ""},
		{"string or dynamic", model.NewUnionType(str, model.DynamicType), ""},
		{"dynamic", model.DynamicType, ""},
		{"none only", model.NoneType, ""},
	} {
		t.Run(tt.name, func(t *testing.T) {
			if got := conversionKind(tt.t); got != tt.want {
				t.Errorf("conversionKind(%v) = %q, want %q", tt.t, got, tt.want)
			}
		})
	}
}

// Cargo package names are narrower than Pulumi project names: a dot is legal
// in a project name and not in a manifest, and cargo refuses the manifest
// rather than the generation that wrote it.
func TestCargoPackageName(t *testing.T) {
	for _, tt := range []struct{ project, want string }{
		{"my-web-app", "my-web-app"},
		{"l2_resource_read", "l2_resource_read"},
		{"MixedCase123", "MixedCase123"},
		{"my.web-app", "my_web-app"},
		{"my project", "my_project"},
		{"org/project", "org_project"},
		{"2fast", "_2fast"},
		{"", "pulumi_program"},
		{"日本語", "___"},
	} {
		if got := cargoPackageName(tt.project); got != tt.want {
			t.Errorf("cargoPackageName(%q) = %q, want %q", tt.project, got, tt.want)
		}
	}
}

// Every package the program references has to appear in the manifest. Outside
// the conformance harness nothing supplies a local artifact, and a manifest
// that omits the crate leaves cargo reporting an unresolved import with
// nothing to point at.
func TestCargoTomlNamesEveryReferencedPackage(t *testing.T) {
	program := bindProgram(t, `
resource "t" "uni:index:Thing" { either = "x" }
`, unionSchema)

	manifest, err := generateProgramCargoToml("my.web-app", program, nil)
	if err != nil {
		t.Fatalf("generate manifest: %v", err)
	}
	got := string(manifest)
	if !strings.Contains(got, `name = "my_web-app"`) {
		t.Errorf("the package name is not a valid cargo name:\n%s", got)
	}
	if !strings.Contains(got, `pulumi_uni = "1.0.0"`) {
		t.Errorf("the referenced package is missing from the manifest:\n%s", got)
	}

	// A local artifact still wins, so the conformance harness is unaffected.
	manifest, err = generateProgramCargoToml("proj", program, map[string]string{"uni": "/tmp/sdk"})
	if err != nil {
		t.Fatalf("generate manifest: %v", err)
	}
	if !strings.Contains(string(manifest), `pulumi_uni = { path = "/tmp/sdk" }`) {
		t.Errorf("a local dependency should stay a path dependency:\n%s", manifest)
	}
}

// A malformed call has to leave through a diagnostic; indexing an argument
// that is not there takes the whole language host down mid-GenerateProject,
// and the CLI reports a transport error instead of the problem.
func TestMissingIntrinsicArgumentsDoNotPanic(t *testing.T) {
	g := newProgramGenerator(&pcl.Program{})
	for _, name := range []string{
		pcl.IntrinsicConvert, pcl.Invoke, "getOutput", "assetArchive",
		"pulumiResourceName", "pulumiResourceType", "recover", "can", "try",
		"secret", "join", "lookup", "element", "min", pcl.Call,
	} {
		t.Run(name, func(t *testing.T) {
			defer func() {
				if r := recover(); r != nil {
					t.Fatalf("%s panicked on missing arguments: %v", name, r)
				}
			}()
			g.functionCallExpr(&model.FunctionCallExpression{
				Name:      name,
				Signature: model.StaticFunctionSignature{ReturnType: model.DynamicType},
			})
		})
	}
}
