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
	"bytes"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"

	"github.com/hashicorp/hcl/v2"
	"github.com/hashicorp/hcl/v2/hclsyntax"
	"github.com/pulumi/pulumi/pkg/v3/codegen/hcl2/model"
	"github.com/pulumi/pulumi/pkg/v3/codegen/pcl"
	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
	"github.com/pulumi/pulumi/sdk/v3/go/common/encoding"
	"github.com/pulumi/pulumi/sdk/v3/go/common/workspace"
	"github.com/zclconf/go-cty/cty"
)

// GenerateProgram generates a Rust Pulumi program from a bound PCL program.
func GenerateProgram(program *pcl.Program) (map[string][]byte, hcl.Diagnostics, error) {
	g := newProgramGenerator(program)
	source, diags := g.generate()
	if diags.HasErrors() {
		return nil, diags, nil
	}
	return map[string][]byte{"src/main.rs": source}, diags, nil
}

// GenerateProject generates a full Rust Pulumi project: Pulumi.yaml, a Cargo
// manifest wired to local SDK artifacts, and the program itself.
func GenerateProject(
	directory string, project workspace.Project,
	program *pcl.Program, localDependencies map[string]string,
) error {
	files, diagnostics, err := GenerateProgram(program)
	if err != nil {
		return err
	}
	if diagnostics.HasErrors() {
		return diagnostics
	}

	rootDirectory := directory
	if project.Main != "" {
		directory = filepath.Join(rootDirectory, project.Main)
		if err := os.MkdirAll(directory, 0o700); err != nil {
			return err
		}
	}

	project.Runtime = workspace.NewProjectRuntimeInfo("rust", nil)
	projectBytes, err := encoding.YAML.Marshal(project)
	if err != nil {
		return err
	}
	if err := os.WriteFile(filepath.Join(rootDirectory, "Pulumi.yaml"), projectBytes, 0o600); err != nil {
		return err
	}

	cargoToml, err := generateProgramCargoToml(project.Name.String(), program, localDependencies)
	if err != nil {
		return err
	}
	files["Cargo.toml"] = cargoToml

	for filename, data := range files {
		outPath := filepath.Join(directory, filename)
		if err := os.MkdirAll(filepath.Dir(outPath), 0o700); err != nil {
			return err
		}
		if err := os.WriteFile(outPath, data, 0o600); err != nil {
			return err
		}
	}
	return nil
}

func generateProgramCargoToml(
	name string, program *pcl.Program, localDependencies map[string]string,
) ([]byte, error) {
	var w bytes.Buffer
	fmt.Fprintf(&w, "[package]\n")
	fmt.Fprintf(&w, "name = %q\n", name)
	fmt.Fprintf(&w, "version = \"0.1.0\"\n")
	fmt.Fprintf(&w, "edition = \"2021\"\n\n")
	fmt.Fprintf(&w, "[dependencies]\n")
	if path, ok := localDependencies["pulumi"]; ok {
		fmt.Fprintf(&w, "pulumi = { path = %q }\n", path)
	} else {
		fmt.Fprintf(&w, "pulumi = \"0.1\"\n")
	}

	packages := program.PackageReferences()
	names := make([]string, 0, len(packages))
	seen := map[string]bool{}
	for _, pkg := range packages {
		if pkg.Name() == "pulumi" || seen[pkg.Name()] {
			continue
		}
		seen[pkg.Name()] = true
		names = append(names, pkg.Name())
	}
	sort.Strings(names)
	for _, pkgName := range names {
		if path, ok := localDependencies[pkgName]; ok {
			fmt.Fprintf(&w, "%s = { path = %q }\n", crateName(pkgName), path)
		}
	}
	fmt.Fprintf(&w, "\n[workspace]\n")
	return w.Bytes(), nil
}

type programGenerator struct {
	program     *pcl.Program
	diagnostics hcl.Diagnostics
	// functionSchemas caches token -> function lookups across packages.
	functionSchemas map[string]*schema.Function
	packages        []*schema.Package
	// builtinVars tracks variables bound to raw pulumi builtin resources
	// (e.g. pulumi:index:Stash), which use dynamic property accessors.
	builtinVars map[string]bool
	// scopeVars maps for-expression iteration variables to closure params.
	scopeVars map[string]string
	// varNames maps PCL variable names to unique Rust identifiers (PCL is
	// case-sensitive; snake_case is not injective).
	varNames map[string]string
	usedVars map[string]bool
	// declaredVars records program variables, for closure capture cloning.
	declaredVars []string
	// forDepth numbers nested for-expressions for unique closure params.
	forDepth int
}

func newProgramGenerator(program *pcl.Program) *programGenerator {
	return &programGenerator{
		program:         program,
		functionSchemas: map[string]*schema.Function{},
		builtinVars:     map[string]bool{},
		scopeVars:       map[string]string{},
		varNames:        map[string]string{},
		usedVars:        map[string]bool{},
	}
}

func (g *programGenerator) errorf(subject hcl.Range, format string, args ...any) {
	g.diagnostics = append(g.diagnostics, &hcl.Diagnostic{
		Severity: hcl.DiagError,
		Summary:  fmt.Sprintf(format, args...),
		Subject:  &subject,
	})
}

func (g *programGenerator) generate() ([]byte, hcl.Diagnostics) {
	pcl.MapProvidersAsResources(g.program)
	nodes := pcl.Linearize(g.program)

	if packages, err := g.program.PackageSnapshots(); err == nil {
		g.packages = packages
	}

	var body bytes.Buffer
	for _, n := range nodes {
		switch n := n.(type) {
		case *pcl.Resource:
			g.genResource(&body, n)
		case *pcl.ConfigVariable:
			g.genConfigVariable(&body, n)
		case *pcl.LocalVariable:
			g.genLocalVariable(&body, n)
		case *pcl.OutputVariable:
			g.genOutputVariable(&body, n)
		case *pcl.ReadResource:
			g.genReadResource(&body, n)
		case *pcl.Component:
			g.errorf(n.Definition.Syntax.DefRange(), "components are not yet supported by the Rust program generator")
		case *pcl.PulumiBlock:
			if n.RequiredVersion != nil {
				fmt.Fprintf(&body, "ctx.require_pulumi_version(%s).await?;\n", g.expr(n.RequiredVersion))
			}
		default:
			// Ignore other nodes (e.g. pulumi version blocks).
		}
	}

	var w bytes.Buffer
	fmt.Fprintf(&w, "// Code generated by pulumi-language-rust. DO NOT EDIT.\n")
	fmt.Fprintf(&w, "#![allow(unused_imports, unused_variables, unused_mut, dead_code, clippy::all)]\n\n")
	fmt.Fprintf(&w, "fn main() {\n")
	fmt.Fprintf(&w, "    pulumi::run(|ctx| async move {\n")
	for _, line := range strings.Split(strings.TrimRight(body.String(), "\n"), "\n") {
		if line == "" {
			w.WriteString("\n")
		} else {
			fmt.Fprintf(&w, "        %s\n", line)
		}
	}
	fmt.Fprintf(&w, "        Ok(())\n")
	fmt.Fprintf(&w, "    });\n")
	fmt.Fprintf(&w, "}\n")
	return w.Bytes(), g.diagnostics
}

// varName renders the Rust variable name for a PCL variable.
func varName(name string) string {
	return escapeIdent(snakeCase(name))
}

// declareVar assigns a unique Rust identifier to a PCL variable name.
// Distinct PCL names that fold to the same snake_case identifier (e.g. mod
// and Mod) get disambiguated.
func (g *programGenerator) declareVar(pclName string) string {
	if existing, ok := g.varNames[pclName]; ok {
		return existing
	}
	candidate := varName(pclName)
	for g.usedVars[candidate] {
		candidate += "_"
	}
	g.varNames[pclName] = candidate
	g.usedVars[candidate] = true
	return candidate
}

// refVar resolves a PCL variable reference to its Rust identifier.
func (g *programGenerator) refVar(pclName string) string {
	if existing, ok := g.varNames[pclName]; ok {
		return existing
	}
	return varName(pclName)
}

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

func (g *programGenerator) genConfigVariable(w *bytes.Buffer, cfg *pcl.ConfigVariable) {
	key := cfg.LogicalName()
	kind := "object"
	switch pcl.UnwrapOption(cfg.Type()) {
	case model.StringType:
		kind = "string"
	case model.NumberType:
		kind = "number"
	case model.IntType:
		kind = "int"
	case model.BoolType:
		kind = "bool"
	}

	var expr string
	if cfg.DefaultValue != nil {
		if def, ok := g.plainPropertyValue(cfg.DefaultValue); ok {
			expr = fmt.Sprintf("ctx.config().get_%s_or(%q, %s)", kind, key, def)
		} else {
			// Dynamic defaults (e.g. computed from an invoke) evaluate
			// lazily, only when the key is unset.
			converted, _ := pcl.RewriteConversions(cfg.DefaultValue, cfg.Type())
			expr = fmt.Sprintf(
				"match ctx.config().get_%s_opt(%q) { Some(v) => v, None => %s }",
				kind, key, g.expr(converted))
		}
	} else if cfg.Nullable {
		expr = fmt.Sprintf("ctx.config().get_%s_or(%q, pulumi::PropertyValue::Null)", kind, key)
	} else {
		expr = fmt.Sprintf("ctx.config().require_%s(%q)?", kind, key)
	}
	if cfg.Secret {
		expr = fmt.Sprintf("pulumi::pv::secret(%s)", expr)
	}
	name := g.declareVar(cfg.Name())
	g.declaredVars = append(g.declaredVars, name)
	fmt.Fprintf(w, "let %s = %s;\n", name, expr)
}

func (g *programGenerator) genLocalVariable(w *bytes.Buffer, local *pcl.LocalVariable) {
	value, _ := pcl.RewriteConversions(local.Definition.Value, local.Type())
	code := g.expr(value)
	name := g.declareVar(local.Name())
	g.declaredVars = append(g.declaredVars, name)
	fmt.Fprintf(w, "let %s = %s;\n", name, code)
}

func (g *programGenerator) genOutputVariable(w *bytes.Buffer, output *pcl.OutputVariable) {
	fmt.Fprintf(w, "ctx.export(%s, %s);\n", rustString(output.LogicalName()), g.expr(output.Value))
}

// resourcePath returns the Rust path of the generated resource struct plus
// its package name.
func (g *programGenerator) resourcePath(r *pcl.Resource) (structPath, pkgName string) {
	token, _ := r.GetToken()
	parts := canonicalTokenParts(token)
	pkgName = parts[0]
	module := parts[1]
	member := parts[2]

	if pkgName == "pulumi" && module == "providers" {
		//

		pkgName = member
		return crateName(pkgName) + "::Provider", pkgName
	}

	crate := crateName(pkgName)
	if module == "index" || module == "" {
		return crate + "::" + pascalCase(member), pkgName
	}
	return crate + "::" + modIdent(module) + "::" + pascalCase(member), pkgName
}

func (g *programGenerator) genResource(w *bytes.Buffer, r *pcl.Resource) {
	if r.Options != nil && r.Options.Range != nil {
		g.errorf(r.Definition.Syntax.DefRange(), "resource range options are not yet supported by the Rust program generator")
		return
	}

	if token, _ := r.GetToken(); token == "pulumi:pulumi:StackReference" {
		g.genStackReference(w, r)
		return
	}
	if token, _ := r.GetToken(); isBuiltinResourceToken(token) {
		g.genBuiltinResource(w, r)
		return
	}

	structPath, _ := g.resourcePath(r)
	name := g.declareVar(r.Name())

	// Insert conversion intrinsics so e.g. string IDs flowing into number
	// properties coerce at runtime, mirroring PCL conversion semantics.
	for _, input := range r.Inputs {
		if destType, tdiags := r.InputType.Traverse(hcl.TraverseAttr{Name: input.Name}); !tdiags.HasErrors() {
			if mt, ok := destType.(model.Type); ok {
				converted, _ := pcl.RewriteConversions(input.Value, mt)
				input.Value = converted
			}
		}
	}

	// Build the args struct literal from the schema's input properties. If
	// the program's expressions don't fit the typed shapes (e.g. inputs
	// computed by for-expressions), fall back to dynamic registration.
	if r.Schema == nil {
		g.errorf(r.Definition.Syntax.DefRange(), "resource %q has no schema", r.Name())
		return
	}
	mark := len(g.diagnostics)
	args := g.typedArgsLiteral(structPath+"Args", r.Schema.InputProperties, r.Inputs, r.Definition.Syntax.DefRange())
	if len(g.diagnostics) > mark {
		g.diagnostics = g.diagnostics[:mark]
		g.genDynamicResource(w, r)
		return
	}

	options := g.resourceOptions(r)

	g.declaredVars = append(g.declaredVars, name)
	fmt.Fprintf(w, "let %s = %s::new(&ctx, %s, %s, %s);\n",
		name, structPath, rustString(r.LogicalName()), args, options)
}

// genDynamicResource registers a schema'd resource through the dynamic
// layer, used when the program's inputs don't fit the typed args shapes.
func (g *programGenerator) genDynamicResource(w *bytes.Buffer, r *pcl.Resource) {
	token, _ := r.GetToken()
	canonical := strings.Join(canonicalTokenParts(token), ":")
	version := ""
	pluginDownloadURL := ""
	if r.Schema != nil {
		if ref := r.Schema.PackageReference; ref != nil {
			if v := ref.Version(); v != nil {
				version = v.String()
			}
			if def, err := ref.Definition(); err == nil && def != nil {
				pluginDownloadURL = def.PluginDownloadURL
			}
		}
	}
	custom := true
	remote := false
	if r.Schema != nil && r.Schema.IsComponent {
		custom = false
		remote = true
	}

	var inputs []string
	for _, input := range r.Inputs {
		inputs = append(inputs, fmt.Sprintf("(%s.to_string(), %s)",
			rustString(input.Name), g.expr(input.Value)))
	}
	name := g.declareVar(r.Name())
	g.builtinVars[name] = true
	g.declaredVars = append(g.declaredVars, name)
	fmt.Fprintf(w, "let %s = ctx.register_resource(pulumi::RegisterRequest { type_: %s.to_string(), name: %s.to_string(), custom: %v, remote: %v, version: %s.to_string(), plugin_download_url: %s.to_string(), inputs: vec![%s], options: %s });\n",
		name, rustString(canonical), rustString(r.LogicalName()), custom, remote,
		rustString(version), rustString(pluginDownloadURL),
		strings.Join(inputs, ", "), g.resourceOptions(r))
}

// canonicalTokenParts splits a type token and normalizes PCL-elided module
// forms (pkg:member, pkg::member) to pkg:index:member.
func canonicalTokenParts(token string) []string {
	parts := strings.Split(token, ":")
	if len(parts) == 2 {
		parts = []string{parts[0], "index", parts[1]}
	}
	if len(parts) == 3 && parts[1] == "" {
		parts[1] = "index"
	}
	return parts
}

// isBuiltinResourceToken reports whether a token names an engine-builtin
// resource served by the pulumi builtin provider (e.g. pulumi:index:Stash).
func isBuiltinResourceToken(token string) bool {
	parts := canonicalTokenParts(token)
	return parts[0] == "pulumi" && parts[1] != "providers" && parts[1] != "pulumi"
}

// genBuiltinResource registers an engine-builtin resource dynamically.
func (g *programGenerator) genBuiltinResource(w *bytes.Buffer, r *pcl.Resource) {
	token, _ := r.GetToken()
	canonical := strings.Join(canonicalTokenParts(token), ":")

	var inputs []string
	for _, input := range r.Inputs {
		inputs = append(inputs, fmt.Sprintf("(%s.to_string(), %s)",
			rustString(input.Name), g.expr(input.Value)))
	}
	name := g.declareVar(r.Name())
	g.builtinVars[name] = true
	g.declaredVars = append(g.declaredVars, name)
	fmt.Fprintf(w, "let %s = ctx.register_resource(pulumi::RegisterRequest { type_: %s.to_string(), name: %s.to_string(), custom: true, remote: false, version: String::new(), plugin_download_url: String::new(), inputs: vec![%s], options: %s });\n",
		name, rustString(canonical), rustString(r.LogicalName()),
		strings.Join(inputs, ", "), g.resourceOptions(r))
}

// genReadResource emits a read of an existing resource's state.
func (g *programGenerator) genReadResource(w *bytes.Buffer, r *pcl.ReadResource) {
	token, _ := r.GetToken()
	canonical := strings.Join(canonicalTokenParts(token), ":")

	version := ""
	if r.Schema != nil && r.Schema.PackageReference != nil {
		if v := r.Schema.PackageReference.Version(); v != nil {
			version = v.String()
		}
	}

	idExpr := "pulumi::pv::null()"
	var inputs []string
	for _, input := range r.Inputs {
		if input.Name == "id" {
			idExpr = g.expr(input.Value)
			continue
		}
		inputs = append(inputs, fmt.Sprintf("(%s.to_string(), %s)",
			rustString(input.Name), g.expr(input.Value)))
	}
	name := varName(r.Name())
	g.builtinVars[name] = true
	fmt.Fprintf(w, "let %s = ctx.read_resource(%s, %s, %s, vec![%s], %s, %s);\n",
		name, rustString(canonical), rustString(r.LogicalName()), idExpr,
		strings.Join(inputs, ", "), rustString(version), g.readResourceOptions(r))
}

// readResourceOptions maps a read resource's options (currently none of the
// interesting ones apply).
func (g *programGenerator) readResourceOptions(r *pcl.ReadResource) string {
	return "pulumi::ResourceOptions::default()"
}

// genStackReference emits a pulumi::StackReference for the builtin
// pulumi:pulumi:StackReference resource type.
func (g *programGenerator) genStackReference(w *bytes.Buffer, r *pcl.Resource) {
	var nameExpr string
	for _, input := range r.Inputs {
		if input.Name == "name" {
			nameExpr = g.expr(input.Value)
		}
	}
	if nameExpr == "" {
		g.errorf(r.Definition.Syntax.DefRange(), "stack reference requires a name input")
		nameExpr = "pulumi::pv::null()"
	}
	name := g.declareVar(r.Name())
	g.builtinVars[name] = true
	g.declaredVars = append(g.declaredVars, name)
	fmt.Fprintf(w, "let %s = pulumi::StackReference::new(&ctx, %s, %s, %s);\n",
		name, rustString(r.LogicalName()), nameExpr, g.resourceOptions(r))
}

// typedArgsLiteral renders `Path { field: value, ... }` for a set of schema
// properties and bound attribute values.
func (g *programGenerator) typedArgsLiteral(
	argsPath string, props []*schema.Property, inputs []*model.Attribute, subject hcl.Range,
) string {
	values := map[string]model.Expression{}
	for _, attr := range inputs {
		values[attr.Name] = attr.Value
	}

	names := fieldNamesFor(props)
	var fields []string
	for _, p := range props {
		name := names[p.Name]
		optional := !p.IsRequired()
		expr, has := values[p.Name]
		if !has {
			if optional {
				fields = append(fields, fmt.Sprintf("%s: None", name))
			} else {
				g.errorf(subject, "missing required input %q", p.Name)
				fields = append(fields, fmt.Sprintf("%s: todo!()", name))
			}
			continue
		}
		value := g.typedInput(expr, p, subject)
		if optional {
			value = "Some(" + value + ")"
		}
		fields = append(fields, fmt.Sprintf("%s: %s", name, value))
	}
	return fmt.Sprintf("%s { %s }", argsPath, strings.Join(fields, ", "))
}

// unwrapConvert strips conversion intrinsics off an expression, exposing the
// literal underneath for shape matching.
func unwrapConvert(expr model.Expression) model.Expression {
	for {
		if call, ok := expr.(*model.FunctionCallExpression); ok && call.Name == pcl.IntrinsicConvert {
			expr = call.Args[0]
			continue
		}
		return expr
	}
}

// typedInput renders an expression for a typed args field.
func (g *programGenerator) typedInput(
	expr model.Expression, p *schema.Property, subject hcl.Range,
) string {
	t := p.Type
	if opt, ok := t.(*schema.OptionalType); ok {
		t = opt.ElementType
	}
	if in, ok := t.(*schema.InputType); ok {
		t = in.ElementType
	}

	// Object-typed inputs become typed args-struct literals. Conversion
	// intrinsics are transparent for shape matching.
	shape := unwrapConvert(expr)
	if obj, ok := t.(*schema.ObjectType); ok {
		return g.typedObjectLiteral(shape, obj, subject)
	}
	if arr, ok := t.(*schema.ArrayType); ok && containsObject(arr.ElementType) {
		if obj, ok := unwrapToObject(arr.ElementType); ok {
			if tuple, ok := shape.(*model.TupleConsExpression); ok {
				var elems []string
				for _, e := range tuple.Expressions {
					elems = append(elems, g.typedObjectLiteral(e, obj, subject))
				}
				return "vec![" + strings.Join(elems, ", ") + "]"
			}
			g.errorf(subject, "expected a list literal for property %q", p.Name)
			return "vec![]"
		}
	}
	if mp, ok := t.(*schema.MapType); ok && containsObject(mp.ElementType) {
		if obj, ok := unwrapToObject(mp.ElementType); ok {
			if object, ok := shape.(*model.ObjectConsExpression); ok {
				var elems []string
				for _, item := range object.Items {
					key, ok := keyString(item.Key)
					if !ok {
						g.errorf(subject, "expected a literal key for property %q", p.Name)
						continue
					}
					elems = append(elems, fmt.Sprintf("(%s.to_string(), %s)",
						rustString(key), g.typedObjectLiteral(item.Value, obj, subject)))
				}
				return "std::collections::BTreeMap::from([" + strings.Join(elems, ", ") + "])"
			}
			g.errorf(subject, "expected a map literal for property %q", p.Name)
			return "std::collections::BTreeMap::new()"
		}
	}

	if p.Plain {
		return g.plainLiteral(expr, t, subject)
	}

	// Everything else is a dynamic output cast to the field's typed form.
	return g.expr(expr) + ".cast()"
}

// typedObjectLiteral renders a typed args-struct literal for an object type.
func (g *programGenerator) typedObjectLiteral(
	expr model.Expression, obj *schema.ObjectType, subject hcl.Range,
) string {
	if obj.IsInputShape() {
		obj = obj.PlainShape
	}
	object, ok := unwrapConvert(expr).(*model.ObjectConsExpression)
	if !ok {
		g.errorf(subject, "expected an object literal for type %q", obj.Token)
		return "Default::default()"
	}
	pg := &pkgGenerator{pkg: packageOfObject(obj)}
	argsPath := g.typesPathFor(obj) + pg.typeNameForToken(obj.Token) + "Args"

	var inputs []*model.Attribute
	for _, item := range object.Items {
		key, ok := keyString(item.Key)
		if !ok {
			g.errorf(subject, "expected a literal key in object literal")
			continue
		}
		inputs = append(inputs, &model.Attribute{Name: key, Value: item.Value})
	}
	return g.typedArgsLiteral(argsPath, obj.Properties, inputs, subject)
}

// typesPathFor computes `pulumi_<pkg>::types::` for an object type.
func (g *programGenerator) typesPathFor(obj *schema.ObjectType) string {
	pkgName := ""
	if pkg, err := obj.PackageReference.Definition(); err == nil && pkg != nil {
		pkgName = pkg.Name
	}
	if pkgName == "" {
		parts := strings.Split(obj.Token, ":")
		pkgName = parts[0]
	}
	return crateName(pkgName) + "::types::"
}

func packageOfObject(obj *schema.ObjectType) *schema.Package {
	if pkg, err := obj.PackageReference.Definition(); err == nil && pkg != nil {
		return pkg
	}
	return &schema.Package{}
}

// plainLiteral renders a plain (non-output) Rust value for a literal
// expression of the given schema type.
func (g *programGenerator) plainLiteral(expr model.Expression, t schema.Type, subject hcl.Range) string {
	expr = unwrapConvert(expr)
	switch t := t.(type) {
	case *schema.OptionalType:
		return g.plainLiteral(expr, t.ElementType, subject)
	case *schema.ArrayType:
		if tuple, ok := expr.(*model.TupleConsExpression); ok {
			var elems []string
			for _, e := range tuple.Expressions {
				elems = append(elems, g.plainLiteral(e, t.ElementType, subject))
			}
			return "vec![" + strings.Join(elems, ", ") + "]"
		}
	case *schema.MapType:
		if object, ok := expr.(*model.ObjectConsExpression); ok {
			var elems []string
			for _, item := range object.Items {
				key, ok := keyString(item.Key)
				if !ok {
					continue
				}
				elems = append(elems, fmt.Sprintf("(%s.to_string(), %s)",
					rustString(key), g.plainLiteral(item.Value, t.ElementType, subject)))
			}
			return "std::collections::BTreeMap::from([" + strings.Join(elems, ", ") + "])"
		}
	}

	switch t {
	case schema.BoolType:
		if lit, ok := expr.(*model.LiteralValueExpression); ok && lit.Value.Type() == cty.Bool {
			return strconv.FormatBool(lit.Value.True())
		}
	case schema.IntType:
		if lit, ok := expr.(*model.LiteralValueExpression); ok && lit.Value.Type() == cty.Number {
			i, _ := lit.Value.AsBigFloat().Int64()
			return strconv.FormatInt(i, 10)
		}
	case schema.NumberType:
		if lit, ok := expr.(*model.LiteralValueExpression); ok && lit.Value.Type() == cty.Number {
			f, _ := lit.Value.AsBigFloat().Float64()
			return formatFloat(f)
		}
	case schema.StringType:
		if s, ok := literalString(expr); ok {
			return rustString(s) + ".to_string()"
		}
	}
	g.errorf(subject, "unsupported plain literal expression")
	return "Default::default()"
}

// alias renders one element of the aliases option: either a literal URN
// string or an object literal naming the parts that differed.
func (g *programGenerator) alias(subject hcl.Range, e model.Expression) string {
	if s, ok := literalString(e); ok {
		return fmt.Sprintf("pulumi::Alias::Urn(%s.to_string())", rustString(s))
	}
	object, ok := unwrapConvert(e).(*model.ObjectConsExpression)
	if !ok {
		g.errorf(subject, "unsupported alias expression")
		return "pulumi::Alias::Spec(pulumi::AliasSpec::default())"
	}
	var parts []string
	for _, item := range object.Items {
		key, ok := keyString(item.Key)
		if !ok {
			g.errorf(subject, "unsupported alias key")
			continue
		}
		switch key {
		case "name", "type", "stack", "project":
			s, ok := literalString(item.Value)
			if !ok {
				g.errorf(subject, "unsupported alias %s", key)
				continue
			}
			field := key
			if key == "type" {
				field = "type_"
			}
			parts = append(parts, fmt.Sprintf("%s: Some(%s.to_string())", field, rustString(s)))
		case "parent":
			res, ok := g.resourceRef(item.Value)
			if !ok {
				g.errorf(subject, "unsupported alias parent")
				continue
			}
			parts = append(parts, fmt.Sprintf(
				"parent: Some(pulumi::AliasParent::Urn(%s.pulumi_resource().clone()))", res))
		case "noParent":
			b, ok := literalBool(item.Value)
			if !ok {
				g.errorf(subject, "unsupported alias noParent")
				continue
			}
			if b {
				parts = append(parts, "parent: Some(pulumi::AliasParent::None)")
			}
		default:
			g.errorf(subject, "unsupported alias key %q", key)
		}
	}
	return fmt.Sprintf("pulumi::Alias::Spec(pulumi::AliasSpec { %s, ..Default::default() })",
		strings.Join(parts, ", "))
}

// resourceOptions renders the pulumi::ResourceOptions literal for a resource.
func (g *programGenerator) resourceOptions(r *pcl.Resource) string {
	opts := r.Options
	if opts == nil {
		return "pulumi::ResourceOptions::default()"
	}
	subject := r.Definition.Syntax.DefRange()

	var fields []string
	setField := func(name, value string) {
		fields = append(fields, fmt.Sprintf("%s: %s", name, value))
	}

	if opts.Parent != nil {
		if res, ok := g.resourceRef(opts.Parent); ok {
			setField("parent", fmt.Sprintf("Some(%s.pulumi_resource().clone())", res))
		} else {
			g.errorf(subject, "unsupported parent expression")
		}
	}
	if opts.Provider != nil {
		if res, ok := g.resourceRef(opts.Provider); ok {
			setField("provider", fmt.Sprintf("Some(%s.pulumi_resource().clone())", res))
		} else {
			g.errorf(subject, "unsupported provider expression")
		}
	}
	if opts.DependsOn != nil {
		if tuple, ok := opts.DependsOn.(*model.TupleConsExpression); ok {
			var elems []string
			for _, e := range tuple.Expressions {
				if res, ok := g.resourceRef(e); ok {
					elems = append(elems, fmt.Sprintf("%s.pulumi_resource().clone()", res))
				} else {
					g.errorf(subject, "unsupported dependsOn element")
				}
			}
			setField("depends_on", "vec!["+strings.Join(elems, ", ")+"]")
		} else {
			g.errorf(subject, "unsupported dependsOn expression")
		}
	}
	if opts.Protect != nil {
		if b, ok := literalBool(opts.Protect); ok {
			setField("protect", fmt.Sprintf("Some(%v)", b))
		} else {
			g.errorf(subject, "unsupported protect expression")
		}
	}
	if opts.RetainOnDelete != nil {
		if b, ok := literalBool(opts.RetainOnDelete); ok {
			setField("retain_on_delete", fmt.Sprintf("Some(%v)", b))
		} else {
			g.errorf(subject, "unsupported retainOnDelete expression")
		}
	}
	if opts.DeleteBeforeReplace != nil {
		if b, ok := literalBool(opts.DeleteBeforeReplace); ok {
			setField("delete_before_replace", fmt.Sprintf("Some(%v)", b))
		} else {
			g.errorf(subject, "unsupported deleteBeforeReplace expression")
		}
	}
	if opts.DeletedWith != nil {
		if res, ok := g.resourceRef(opts.DeletedWith); ok {
			setField("deleted_with", fmt.Sprintf("Some(%s.pulumi_resource().clone())", res))
		} else {
			g.errorf(subject, "unsupported deletedWith expression")
		}
	}
	if opts.IgnoreChanges != nil {
		if elems, ok := g.stringList(opts.IgnoreChanges); ok {
			setField("ignore_changes", elems)
		} else {
			g.errorf(subject, "unsupported ignoreChanges expression")
		}
	}
	if opts.AdditionalSecretOutputs != nil {
		if elems, ok := g.stringList(opts.AdditionalSecretOutputs); ok {
			setField("additional_secret_outputs", elems)
		} else {
			g.errorf(subject, "unsupported additionalSecretOutputs expression")
		}
	}
	if opts.ReplaceOnChanges != nil {
		if elems, ok := g.stringList(opts.ReplaceOnChanges); ok {
			setField("replace_on_changes", elems)
		} else {
			g.errorf(subject, "unsupported replaceOnChanges expression")
		}
	}
	if opts.Version != nil {
		if s, ok := literalString(opts.Version); ok {
			setField("version", rustString(s)+".to_string()")
		} else {
			g.errorf(subject, "unsupported version expression")
		}
	}
	if opts.PluginDownloadURL != nil {
		if s, ok := literalString(opts.PluginDownloadURL); ok {
			setField("plugin_download_url", rustString(s)+".to_string()")
		} else {
			g.errorf(subject, "unsupported pluginDownloadURL expression")
		}
	}
	if opts.ImportID != nil {
		if s, ok := literalString(opts.ImportID); ok {
			setField("import_id", rustString(s)+".to_string()")
		} else {
			g.errorf(subject, "unsupported import expression")
		}
	}
	if opts.CustomTimeouts != nil {
		if object, ok := unwrapConvert(opts.CustomTimeouts).(*model.ObjectConsExpression); ok {
			var parts []string
			for _, item := range object.Items {
				key, ok := keyString(item.Key)
				if !ok {
					g.errorf(subject, "unsupported customTimeouts key")
					continue
				}
				parts = append(parts, fmt.Sprintf("%s: Some(%s)", escapeIdent(key), g.expr(item.Value)))
			}
			setField("custom_timeouts", fmt.Sprintf(
				"Some(pulumi::CustomTimeouts { %s, ..Default::default() })", strings.Join(parts, ", ")))
		} else {
			g.errorf(subject, "unsupported customTimeouts expression")
		}
	}

	if opts.HideDiffs != nil {
		if elems, ok := g.stringList(opts.HideDiffs); ok {
			setField("hide_diffs", elems)
		} else {
			g.errorf(subject, "unsupported hideDiffs expression")
		}
	}
	if opts.ReplaceWith != nil {
		if tuple, ok := unwrapConvert(opts.ReplaceWith).(*model.TupleConsExpression); ok {
			var elems []string
			for _, e := range tuple.Expressions {
				if res, ok := g.resourceRef(e); ok {
					elems = append(elems, fmt.Sprintf("%s.pulumi_resource().clone()", res))
				} else {
					g.errorf(subject, "unsupported replaceWith element")
				}
			}
			setField("replace_with", "vec!["+strings.Join(elems, ", ")+"]")
		} else {
			g.errorf(subject, "unsupported replaceWith expression")
		}
	}
	if opts.ReplacementTrigger != nil {
		setField("replacement_trigger", fmt.Sprintf("Some(%s)", g.expr(opts.ReplacementTrigger)))
	}
	if opts.EnvVarMappings != nil {
		if object, ok := unwrapConvert(opts.EnvVarMappings).(*model.ObjectConsExpression); ok {
			var elems []string
			for _, item := range object.Items {
				key, keyOK := keyString(item.Key)
				val, valOK := literalString(item.Value)
				if !keyOK || !valOK {
					g.errorf(subject, "unsupported envVarMappings entry")
					continue
				}
				elems = append(elems, fmt.Sprintf("(%s.to_string(), %s.to_string())",
					rustString(key), rustString(val)))
			}
			setField("env_var_mappings", "vec!["+strings.Join(elems, ", ")+"]")
		} else {
			g.errorf(subject, "unsupported envVarMappings expression")
		}
	}
	if opts.Aliases != nil {
		if tuple, ok := unwrapConvert(opts.Aliases).(*model.TupleConsExpression); ok {
			var elems []string
			for _, e := range tuple.Expressions {
				elems = append(elems, g.alias(subject, e))
			}
			setField("aliases", "vec!["+strings.Join(elems, ", ")+"]")
		} else {
			g.errorf(subject, "unsupported aliases expression")
		}
	}

	if opts.Providers != nil {
		var elems []string
		appendProvider := func(key string, e model.Expression) {
			if res, ok := g.resourceRef(e); ok {
				elems = append(elems, fmt.Sprintf("(%s.to_string(), %s.pulumi_resource().clone())",
					rustString(key), res))
			} else {
				g.errorf(subject, "unsupported providers element")
			}
		}
		switch v := unwrapConvert(opts.Providers).(type) {
		case *model.TupleConsExpression:
			for _, e := range v.Expressions {
				if _, r, ok := g.resourceRefNode(e); ok {
					_, pkg := g.resourcePath(r)
					appendProvider(pkg, e)
				} else {
					g.errorf(subject, "unsupported providers element")
				}
			}
		case *model.ObjectConsExpression:
			for _, item := range v.Items {
				key, ok := keyString(item.Key)
				if !ok {
					g.errorf(subject, "unsupported providers key")
					continue
				}
				appendProvider(key, item.Value)
			}
		default:
			g.errorf(subject, "unsupported providers expression")
		}
		setField("providers", "vec!["+strings.Join(elems, ", ")+"]")
	}

	unsupported := []struct {
		name string
		expr model.Expression
	}{
		{"hooks", opts.Hooks},
	}
	for _, u := range unsupported {
		if u.expr != nil {
			g.errorf(subject, "resource option %q is not yet supported by the Rust program generator", u.name)
		}
	}

	if len(fields) == 0 {
		return "pulumi::ResourceOptions::default()"
	}
	return fmt.Sprintf("pulumi::ResourceOptions { %s, ..Default::default() }", strings.Join(fields, ", "))
}

func (g *programGenerator) stringList(expr model.Expression) (string, bool) {
	tuple, ok := unwrapConvert(expr).(*model.TupleConsExpression)
	if !ok {
		return "", false
	}
	var elems []string
	for _, e := range tuple.Expressions {
		s, ok := literalString(e)
		if !ok {
			// Property selectors may be written as bare traversals, e.g.
			// ignoreChanges = [data.value].
			s, ok = traversalPath(e)
			if !ok {
				return "", false
			}
		}
		elems = append(elems, rustString(s)+".to_string()")
	}
	return "vec![" + strings.Join(elems, ", ") + "]", true
}

// traversalPath renders a bare traversal expression as a Pulumi property
// path string (e.g. details[0].key, tags["with.dot"]).
func traversalPath(expr model.Expression) (string, bool) {
	scope, ok := unwrapConvert(expr).(*model.ScopeTraversalExpression)
	if !ok {
		return "", false
	}
	var b strings.Builder
	b.WriteString(scope.RootName)
	writeKey := func(key string) {
		if isPlainPathIdent(key) {
			b.WriteString(".")
			b.WriteString(key)
		} else {
			b.WriteString("[\"")
			b.WriteString(strings.ReplaceAll(key, "\"", "\\\""))
			b.WriteString("\"]")
		}
	}
	for _, t := range scope.Traversal[1:] {
		switch t := t.(type) {
		case hcl.TraverseAttr:
			writeKey(t.Name)
		case hcl.TraverseIndex:
			if t.Key.Type() == cty.String {
				writeKey(t.Key.AsString())
			} else {
				i, _ := t.Key.AsBigFloat().Int64()
				b.WriteString("[")
				b.WriteString(strconv.FormatInt(i, 10))
				b.WriteString("]")
			}
		default:
			return "", false
		}
	}
	return b.String(), true
}

// isPlainPathIdent reports whether a property-path key needs no quoting.
func isPlainPathIdent(s string) bool {
	if s == "" {
		return false
	}
	for i, r := range s {
		switch {
		case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r == '_':
		case r >= '0' && r <= '9':
			if i == 0 {
				return false
			}
		default:
			return false
		}
	}
	return true
}

// resourceRef resolves an expression referring to a resource variable.
func (g *programGenerator) resourceRef(expr model.Expression) (string, bool) {
	name, _, ok := g.resourceRefNode(expr)
	return name, ok
}

// resourceRefNode resolves a resource reference to its variable name and
// bound resource node.
func (g *programGenerator) resourceRefNode(expr model.Expression) (string, *pcl.Resource, bool) {
	scope, ok := unwrapConvert(expr).(*model.ScopeTraversalExpression)
	if !ok {
		return "", nil, false
	}
	if len(scope.Parts) > 0 {
		if r, ok := scope.Parts[0].(*pcl.Resource); ok {
			return g.refVar(scope.RootName), r, true
		}
	}
	return "", nil, false
}

// ---------------------------------------------------------------------------
// Expressions (dynamic space: everything is Output<PropertyValue>)
// ---------------------------------------------------------------------------

func (g *programGenerator) expr(expr model.Expression) string {
	switch expr := expr.(type) {
	case *model.LiteralValueExpression:
		return g.literalExpr(expr)
	case *model.TemplateExpression:
		return g.templateExpr(expr)
	case *model.TupleConsExpression:
		var elems []string
		for _, e := range expr.Expressions {
			elems = append(elems, g.expr(e))
		}
		return "pulumi::pv::array(vec![" + strings.Join(elems, ", ") + "])"
	case *model.ObjectConsExpression:
		var elems []string
		for _, item := range expr.Items {
			key, ok := keyString(item.Key)
			if !ok {
				g.errorf(expr.SyntaxNode().Range(), "unsupported non-literal object key")
				continue
			}
			elems = append(elems, fmt.Sprintf("(%s.to_string(), %s)", rustString(key), g.expr(item.Value)))
		}
		return "pulumi::pv::object(vec![" + strings.Join(elems, ", ") + "])"
	case *model.ScopeTraversalExpression:
		return g.scopeTraversalExpr(expr)
	case *model.RelativeTraversalExpression:
		return g.traversalChain(g.expr(expr.Source), expr.Traversal)
	case *model.FunctionCallExpression:
		return g.functionCallExpr(expr)
	case *model.BinaryOpExpression:
		return g.binaryOpExpr(expr)
	case *model.UnaryOpExpression:
		return g.unaryOpExpr(expr)
	case *model.ConditionalExpression:
		return fmt.Sprintf("pulumi::ops::cond(%s, %s, %s)",
			g.expr(expr.Condition), g.expr(expr.TrueResult), g.expr(expr.FalseResult))
	case *model.IndexExpression:
		return fmt.Sprintf("pulumi::ops::index(%s, %s)", g.expr(expr.Collection), g.expr(expr.Key))
	case *model.ForExpression:
		return g.forExpr(expr)
	case *model.SplatExpression:
		return g.splatExpr(expr)
	}
	g.errorf(expr.SyntaxNode().Range(), "unsupported expression %T", expr)
	return "pulumi::pv::null()"
}

func (g *programGenerator) literalExpr(expr *model.LiteralValueExpression) string {
	v := expr.Value
	if v.IsNull() {
		return "pulumi::pv::null()"
	}
	switch v.Type() {
	case cty.String:
		return fmt.Sprintf("pulumi::pv::string(%s)", rustString(v.AsString()))
	case cty.Number:
		f, _ := v.AsBigFloat().Float64()
		return fmt.Sprintf("pulumi::pv::number(%s)", formatFloat(f))
	case cty.Bool:
		return fmt.Sprintf("pulumi::pv::bool(%v)", v.True())
	}
	g.errorf(expr.SyntaxNode().Range(), "unsupported literal value")
	return "pulumi::pv::null()"
}

func (g *programGenerator) templateExpr(expr *model.TemplateExpression) string {
	if len(expr.Parts) == 1 {
		if lit, ok := expr.Parts[0].(*model.LiteralValueExpression); ok && lit.Value.Type() == cty.String {
			return fmt.Sprintf("pulumi::pv::string(%s)", rustString(lit.Value.AsString()))
		}
	}
	var parts []string
	for _, part := range expr.Parts {
		parts = append(parts, g.expr(part))
	}
	return "pulumi::pv::concat(vec![" + strings.Join(parts, ", ") + "])"
}

func (g *programGenerator) scopeTraversalExpr(expr *model.ScopeTraversalExpression) string {
	if len(expr.Parts) == 0 {
		g.errorf(expr.SyntaxNode().Range(), "empty traversal")
		return "pulumi::pv::null()"
	}
	rest := expr.Traversal[1:]
	switch root := expr.Parts[0].(type) {
	case *pcl.Resource, *pcl.ReadResource:
		res := g.refVar(expr.RootName)
		if len(rest) == 0 {
			// A bare resource reference: surface its URN.
			return res + ".urn().cast::<pulumi::PropertyValue>()"
		}
		attr, ok := rest[0].(hcl.TraverseAttr)
		if !ok {
			g.errorf(expr.SyntaxNode().Range(), "unsupported resource traversal")
			return "pulumi::pv::null()"
		}
		var base string
		switch {
		case attr.Name == "urn":
			base = res + ".urn().cast::<pulumi::PropertyValue>()"
		case attr.Name == "id":
			base = res + ".id().cast::<pulumi::PropertyValue>()"
		case g.builtinVars[res]:
			base = fmt.Sprintf("%s.output(%s)", res, rustString(attr.Name))
		default:
			base = fmt.Sprintf("%s.%s().cast::<pulumi::PropertyValue>()", res, fieldName(attr.Name))
		}
		_ = root
		return g.traversalChain(base, rest[1:])
	case *pcl.ConfigVariable, *pcl.LocalVariable:
		base := g.refVar(expr.RootName) + ".clone()"
		return g.traversalChain(base, rest)
	case *model.Variable:
		if mapped, ok := g.scopeVars[expr.RootName]; ok {
			return g.traversalChain(mapped+".clone()", rest)
		}
	case *model.SplatVariable:
		if mapped, ok := g.scopeVars[expr.RootName]; ok {
			return g.traversalChain(mapped+".clone()", rest)
		}
	case *pcl.OutputVariable:
		g.errorf(expr.SyntaxNode().Range(), "output variable references are not supported")
		return "pulumi::pv::null()"
	}
	g.errorf(expr.SyntaxNode().Range(), "unsupported variable reference %q", expr.RootName)
	return "pulumi::pv::null()"
}

func (g *programGenerator) traversalChain(base string, traversal hcl.Traversal) string {
	out := base
	for _, part := range traversal {
		switch part := part.(type) {
		case hcl.TraverseAttr:
			out = fmt.Sprintf("%s.index(%s)", out, rustString(part.Name))
		case hcl.TraverseIndex:
			switch part.Key.Type() {
			case cty.Number:
				i, _ := part.Key.AsBigFloat().Int64()
				out = fmt.Sprintf("%s.index(%dusize)", out, i)
			case cty.String:
				out = fmt.Sprintf("%s.index(%s)", out, rustString(part.Key.AsString()))
			}
		}
	}
	return out
}

func (g *programGenerator) binaryOpExpr(expr *model.BinaryOpExpression) string {
	var op string
	switch expr.Operation {
	case hclsyntax.OpAdd:
		op = "add"
	case hclsyntax.OpSubtract:
		op = "sub"
	case hclsyntax.OpMultiply:
		op = "mul"
	case hclsyntax.OpDivide:
		op = "div"
	case hclsyntax.OpModulo:
		op = "rem"
	case hclsyntax.OpEqual:
		op = "eq"
	case hclsyntax.OpNotEqual:
		op = "neq"
	case hclsyntax.OpLessThan:
		op = "lt"
	case hclsyntax.OpLessThanOrEqual:
		op = "lte"
	case hclsyntax.OpGreaterThan:
		op = "gt"
	case hclsyntax.OpGreaterThanOrEqual:
		op = "gte"
	case hclsyntax.OpLogicalAnd:
		op = "and"
	case hclsyntax.OpLogicalOr:
		op = "or"
	default:
		g.errorf(expr.SyntaxNode().Range(), "unsupported binary operation")
		return "pulumi::pv::null()"
	}
	return fmt.Sprintf("pulumi::ops::%s(%s, %s)", op, g.expr(expr.LeftOperand), g.expr(expr.RightOperand))
}

func (g *programGenerator) unaryOpExpr(expr *model.UnaryOpExpression) string {
	switch expr.Operation {
	case hclsyntax.OpNegate:
		return fmt.Sprintf("pulumi::ops::neg(%s)", g.expr(expr.Operand))
	case hclsyntax.OpLogicalNot:
		return fmt.Sprintf("pulumi::ops::not(%s)", g.expr(expr.Operand))
	}
	g.errorf(expr.SyntaxNode().Range(), "unsupported unary operation")
	return "pulumi::pv::null()"
}

func (g *programGenerator) functionCallExpr(expr *model.FunctionCallExpression) string {
	subject := expr.SyntaxNode().Range()
	arg := func(i int) string {
		if i < len(expr.Args) {
			return g.expr(expr.Args[i])
		}
		return "pulumi::pv::null()"
	}
	switch expr.Name {
	case pcl.IntrinsicConvert:
		inner := g.expr(expr.Args[0])
		switch conversionKind(expr.Type()) {
		case "number":
			return fmt.Sprintf("pulumi::ops::to_number(%s)", inner)
		case "int":
			return fmt.Sprintf("pulumi::ops::to_int(%s)", inner)
		case "bool":
			return fmt.Sprintf("pulumi::ops::to_bool(%s)", inner)
		case "string":
			return fmt.Sprintf("pulumi::ops::to_string(%s)", inner)
		}
		return inner
	case pcl.Invoke:
		return g.invokeExpr(expr)
	case "getOutput":
		if scope, ok := expr.Args[0].(*model.ScopeTraversalExpression); ok && len(scope.Parts) > 0 {
			if _, isRes := scope.Parts[0].(*pcl.Resource); isRes {
				return fmt.Sprintf("%s.get_output(%s)", g.refVar(scope.RootName), arg(1))
			}
		}
		g.errorf(subject, "getOutput requires a stack reference variable")
		return "pulumi::pv::null()"
	case "secret":
		return fmt.Sprintf("pulumi::pv::secret(%s)", arg(0))
	case "unsecret":
		return fmt.Sprintf("pulumi::pv::unsecret(%s)", arg(0))
	case "stack":
		return "pulumi::pv::string(ctx.stack())"
	case "project":
		return "pulumi::pv::string(ctx.project())"
	case "organization":
		return "pulumi::pv::string(ctx.organization())"
	case "cwd":
		return "pulumi::pv::cwd()"
	case "rootDirectory":
		return "pulumi::pv::string(std::env::var(\"PULUMI_ROOT_DIRECTORY\").unwrap_or_default())"
	case "fileAsset":
		return fmt.Sprintf("pulumi::pv::file_asset(%s)", arg(0))
	case "stringAsset":
		return fmt.Sprintf("pulumi::pv::string_asset(%s)", arg(0))
	case "remoteAsset":
		return fmt.Sprintf("pulumi::pv::remote_asset(%s)", arg(0))
	case "fileArchive":
		return fmt.Sprintf("pulumi::pv::file_archive(%s)", arg(0))
	case "remoteArchive":
		return fmt.Sprintf("pulumi::pv::remote_archive(%s)", arg(0))
	case "assetArchive":
		if object, ok := expr.Args[0].(*model.ObjectConsExpression); ok {
			var elems []string
			for _, item := range object.Items {
				key, ok := keyString(item.Key)
				if !ok {
					continue
				}
				elems = append(elems, fmt.Sprintf("(%s.to_string(), %s)", rustString(key), g.expr(item.Value)))
			}
			return "pulumi::pv::asset_archive(vec![" + strings.Join(elems, ", ") + "])"
		}
		g.errorf(subject, "assetArchive requires an object literal")
		return "pulumi::pv::null()"
	case "readFile":
		return fmt.Sprintf("pulumi::pv::read_file(%s)", arg(0))
	case "filebase64":
		return fmt.Sprintf("pulumi::pv::file_base64(%s)", arg(0))
	case "filebase64sha256":
		return fmt.Sprintf("pulumi::pv::file_base64_sha256(%s)", arg(0))
	case "sha1":
		return fmt.Sprintf("pulumi::pv::sha1_hex(%s)", arg(0))
	case "toBase64":
		return fmt.Sprintf("pulumi::pv::to_base64(%s)", arg(0))
	case "fromBase64":
		return fmt.Sprintf("pulumi::pv::from_base64(%s)", arg(0))
	case "toJSON":
		return fmt.Sprintf("pulumi::pv::to_json(%s)", arg(0))
	case "join":
		return fmt.Sprintf("pulumi::pv::join(%s, %s)", arg(0), arg(1))
	case "split":
		return fmt.Sprintf("pulumi::pv::split(%s, %s)", arg(0), arg(1))
	case "length":
		return fmt.Sprintf("pulumi::pv::length(%s)", arg(0))
	case "element":
		return fmt.Sprintf("pulumi::pv::element(%s, %s)", arg(0), arg(1))
	case "entries":
		return fmt.Sprintf("pulumi::pv::entries(%s)", arg(0))
	case "pulumiResourceName":
		if res, ok := g.resourceRef(expr.Args[0]); ok {
			return fmt.Sprintf("pulumi::pv::urn_name(%s.urn().cast::<pulumi::PropertyValue>())", res)
		}
		return fmt.Sprintf("pulumi::pv::urn_name(%s)", arg(0))
	case "pulumiResourceType":
		if res, ok := g.resourceRef(expr.Args[0]); ok {
			return fmt.Sprintf("pulumi::pv::urn_type(%s.urn().cast::<pulumi::PropertyValue>())", res)
		}
		return fmt.Sprintf("pulumi::pv::urn_type(%s)", arg(0))
	case "singleOrNone":
		return fmt.Sprintf("pulumi::pv::single_or_none(%s)", arg(0))
	case "lookup":
		def := "pulumi::pv::null()"
		if len(expr.Args) > 2 {
			def = g.expr(expr.Args[2])
		}
		return fmt.Sprintf("pulumi::pv::lookup(%s, %s, %s)", arg(0), arg(1), def)
	case "min", "max":
		var elems []string
		for _, e := range expr.Args {
			elems = append(elems, g.expr(e))
		}
		return fmt.Sprintf("pulumi::pv::%s(vec![%s])", expr.Name, strings.Join(elems, ", "))
	}
	g.errorf(subject, "function %q is not yet supported by the Rust program generator", expr.Name)
	return "pulumi::pv::null()"
}

// forExpr renders a PCL for-expression via runtime helpers, with iteration
// variables mapped to closure parameters.
func (g *programGenerator) forExpr(expr *model.ForExpression) string {
	subject := expr.SyntaxNode().Range()
	if expr.Group {
		g.errorf(subject, "grouped for-expressions are not yet supported")
		return "pulumi::pv::null()"
	}

	depth := g.forDepth
	g.forDepth++
	kParam := fmt.Sprintf("__k%d", depth)
	vParam := fmt.Sprintf("__v%d", depth)

	var savedKey, savedVal string
	var hadKey, hadVal bool
	if expr.KeyVariable != nil {
		savedKey, hadKey = g.scopeVars[expr.KeyVariable.Name]
		g.scopeVars[expr.KeyVariable.Name] = kParam
	}
	if expr.ValueVariable != nil {
		savedVal, hadVal = g.scopeVars[expr.ValueVariable.Name]
		g.scopeVars[expr.ValueVariable.Name] = vParam
	}

	cond := "pulumi::pv::bool(true)"
	if expr.Condition != nil {
		cond = g.expr(expr.Condition)
	}
	var keyBody string
	if expr.Key != nil {
		keyBody = g.expr(expr.Key)
	}
	valueBody := g.expr(expr.Value)

	if expr.KeyVariable != nil {
		if hadKey {
			g.scopeVars[expr.KeyVariable.Name] = savedKey
		} else {
			delete(g.scopeVars, expr.KeyVariable.Name)
		}
	}
	if expr.ValueVariable != nil {
		if hadVal {
			g.scopeVars[expr.ValueVariable.Name] = savedVal
		} else {
			delete(g.scopeVars, expr.ValueVariable.Name)
		}
	}
	g.forDepth--

	coll := g.expr(expr.Collection)

	prefix := g.captureClones([]string{cond, keyBody, valueBody})

	closure := func(body string) string {
		return fmt.Sprintf(
			"{ %s move |%s: pulumi::Output<pulumi::PropertyValue>, %s: pulumi::Output<pulumi::PropertyValue>| %s }",
			prefix, kParam, vParam, body)
	}

	if expr.Key != nil {
		return fmt.Sprintf("pulumi::ops::for_object(%s, %s, %s, %s)",
			coll, closure(cond), closure(keyBody), closure(valueBody))
	}
	return fmt.Sprintf("pulumi::ops::for_array(%s, %s, %s)", coll, closure(cond), closure(valueBody))
}

// seenIn reports whether a clone statement for v was already emitted.
func seenIn(clones []string, v string) bool {
	needle := "let " + v + " = "
	for _, c := range clones {
		if strings.HasPrefix(c, needle) {
			return true
		}
	}
	return false
}

// containsIdent reports whether ident appears in code as a whole word.
func containsIdent(code, ident string) bool {
	idx := 0
	for {
		i := strings.Index(code[idx:], ident)
		if i < 0 {
			return false
		}
		i += idx
		before := byte(' ')
		if i > 0 {
			before = code[i-1]
		}
		afterIdx := i + len(ident)
		after := byte(' ')
		if afterIdx < len(code) {
			after = code[afterIdx]
		}
		isWord := func(c byte) bool {
			return c == '_' || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')
		}
		if !isWord(before) && !isWord(after) {
			return true
		}
		idx = i + len(ident)
	}
}

// splatExpr renders source[*].attr as a runtime map over the source list.
func (g *programGenerator) splatExpr(expr *model.SplatExpression) string {
	depth := g.forDepth
	g.forDepth++
	kParam := fmt.Sprintf("__k%d", depth)
	vParam := fmt.Sprintf("__v%d", depth)

	saved, had := g.scopeVars[expr.Item.Name]
	g.scopeVars[expr.Item.Name] = vParam
	each := g.expr(expr.Each)
	if had {
		g.scopeVars[expr.Item.Name] = saved
	} else {
		delete(g.scopeVars, expr.Item.Name)
	}
	g.forDepth--

	coll := g.expr(expr.Source)
	prefix := g.captureClones([]string{each})
	closure := func(body string) string {
		return fmt.Sprintf(
			"{ %s move |%s: pulumi::Output<pulumi::PropertyValue>, %s: pulumi::Output<pulumi::PropertyValue>| %s }",
			prefix, kParam, vParam, body)
	}
	return fmt.Sprintf("pulumi::ops::for_array(%s, %s, %s)",
		coll, closure("pulumi::pv::bool(true)"), closure(each))
}

// captureClones emits clone statements for program variables referenced in
// closure bodies so move closures don't consume the originals.
func (g *programGenerator) captureClones(bodies []string) string {
	captured := map[string]bool{}
	candidates := append([]string{"ctx"}, g.declaredVars...)
	// Enclosing for/splat closure parameters are captured too when nested.
	for _, mapped := range g.scopeVars {
		candidates = append(candidates, mapped)
	}
	sort.Strings(candidates[1:])
	for _, body := range bodies {
		for _, v := range candidates {
			if containsIdent(body, v) {
				captured[v] = true
			}
		}
	}
	var clones []string
	for _, v := range candidates {
		if captured[v] && !seenIn(clones, v) {
			clones = append(clones, fmt.Sprintf("let %s = %s.clone();", v, v))
		}
	}
	return strings.Join(clones, " ")
}

// invokeExpr renders a typed invoke call.
func (g *programGenerator) invokeExpr(expr *model.FunctionCallExpression) string {
	subject := expr.SyntaxNode().Range()
	token, ok := literalString(expr.Args[0])
	if !ok {
		g.errorf(subject, "invoke token must be a literal string")
		return "pulumi::pv::null()"
	}
	parts := canonicalTokenParts(token)
	canonical := strings.Join(parts, ":")
	fn := g.lookupFunction(canonical)
	if fn == nil {
		g.errorf(subject, "unknown function %q", canonical)
		return "pulumi::pv::null()"
	}

	pkgName, module, member := parts[0], parts[1], parts[2]
	crate := crateName(pkgName)
	fnPath := crate + "::" + functionName(member)
	argsPath := crate + "::" + pascalCase(member) + "Args"
	if module != "index" && module != "" {
		fnPath = crate + "::" + modIdent(module) + "::" + functionName(member)
		argsPath = crate + "::" + modIdent(module) + "::" + pascalCase(member) + "Args"
	}

	var props []*schema.Property
	if fn.Inputs != nil {
		props = fn.Inputs.Properties
	}
	var inputs []*model.Attribute
	if len(expr.Args) > 1 {
		// Insert conversion intrinsics against the function's declared
		// argument type so config-driven values coerce correctly.
		if len(expr.Signature.Parameters) > 1 {
			converted, _ := pcl.RewriteConversions(expr.Args[1], expr.Signature.Parameters[1].Type)
			expr.Args[1] = converted
		}
		if object, ok := unwrapConvert(expr.Args[1]).(*model.ObjectConsExpression); ok {
			for _, item := range object.Items {
				key, ok := keyString(item.Key)
				if !ok {
					g.errorf(subject, "invoke arguments must use literal keys")
					continue
				}
				inputs = append(inputs, &model.Attribute{Name: key, Value: item.Value})
			}
		}
	}
	args := g.typedArgsLiteral(argsPath, props, inputs, subject)

	options := "pulumi::InvokeOptions::default()"
	if len(expr.Args) > 2 {
		options = g.invokeOptions(expr.Args[2], subject)
	}

	return fmt.Sprintf("%s(&ctx, %s, %s).cast::<pulumi::PropertyValue>()", fnPath, args, options)
}

func (g *programGenerator) invokeOptions(expr model.Expression, subject hcl.Range) string {
	object, ok := expr.(*model.ObjectConsExpression)
	if !ok {
		g.errorf(subject, "invoke options must be an object literal")
		return "pulumi::InvokeOptions::default()"
	}
	var fields []string
	for _, item := range object.Items {
		key, ok := keyString(item.Key)
		if !ok {
			continue
		}
		switch key {
		case "provider":
			if res, ok := g.resourceRef(item.Value); ok {
				fields = append(fields, fmt.Sprintf("provider: Some(%s.pulumi_resource().clone())", res))
			} else {
				g.errorf(subject, "unsupported invoke provider expression")
			}
		case "parent":
			if res, ok := g.resourceRef(item.Value); ok {
				fields = append(fields, fmt.Sprintf("parent: Some(%s.pulumi_resource().clone())", res))
			}
		case "version":
			if s, ok := literalString(item.Value); ok {
				fields = append(fields, fmt.Sprintf("version: %s.to_string()", rustString(s)))
			}
		case "pluginDownloadUrl", "pluginDownloadURL":
			if s, ok := literalString(item.Value); ok {
				fields = append(fields, fmt.Sprintf("plugin_download_url: %s.to_string()", rustString(s)))
			}
		case "dependsOn":
			if tuple, ok := unwrapConvert(item.Value).(*model.TupleConsExpression); ok {
				var elems []string
				for _, e := range tuple.Expressions {
					if res, ok := g.resourceRef(e); ok {
						elems = append(elems, fmt.Sprintf("%s.pulumi_resource().clone()", res))
					} else {
						g.errorf(subject, "unsupported invoke dependsOn element")
					}
				}
				fields = append(fields, "depends_on: vec!["+strings.Join(elems, ", ")+"]")
			} else {
				g.errorf(subject, "unsupported invoke dependsOn expression")
			}
		default:
			g.errorf(subject, "unsupported invoke option %q", key)
		}
	}
	if len(fields) == 0 {
		return "pulumi::InvokeOptions::default()"
	}
	return fmt.Sprintf("pulumi::InvokeOptions { %s, ..Default::default() }", strings.Join(fields, ", "))
}

func (g *programGenerator) lookupFunction(token string) *schema.Function {
	if fn, ok := g.functionSchemas[token]; ok {
		return fn
	}
	for _, pkg := range g.packages {
		for _, fn := range pkg.Functions {
			if fn.Token == token {
				g.functionSchemas[token] = fn
				return fn
			}
		}
	}
	// Fall back to package + member matching: schemas with custom module
	// formats publish tokens that differ from the PCL-normalized form.
	parts := strings.Split(token, ":")
	if len(parts) == 3 {
		for _, pkg := range g.packages {
			if pkg.Name != parts[0] {
				continue
			}
			for _, fn := range pkg.Functions {
				if tokenMember(fn.Token) == parts[2] {
					g.functionSchemas[token] = fn
					return fn
				}
			}
		}
	}
	return nil
}

// plainPropertyValue renders a literal expression as a plain
// pulumi::PropertyValue constructor (used for config defaults).
func (g *programGenerator) plainPropertyValue(expr model.Expression) (string, bool) {
	switch expr := expr.(type) {
	case *model.LiteralValueExpression:
		v := expr.Value
		if v.IsNull() {
			return "pulumi::PropertyValue::Null", true
		}
		switch v.Type() {
		case cty.String:
			return fmt.Sprintf("pulumi::PropertyValue::String(%s.to_string())", rustString(v.AsString())), true
		case cty.Number:
			f, _ := v.AsBigFloat().Float64()
			return fmt.Sprintf("pulumi::PropertyValue::Number(%s)", formatFloat(f)), true
		case cty.Bool:
			return fmt.Sprintf("pulumi::PropertyValue::Bool(%v)", v.True()), true
		}
	case *model.TemplateExpression:
		if s, ok := literalString(expr); ok {
			return fmt.Sprintf("pulumi::PropertyValue::String(%s.to_string())", rustString(s)), true
		}
	case *model.TupleConsExpression:
		var elems []string
		for _, e := range expr.Expressions {
			v, ok := g.plainPropertyValue(e)
			if !ok {
				return "", false
			}
			elems = append(elems, v)
		}
		return "pulumi::PropertyValue::Array(vec![" + strings.Join(elems, ", ") + "])", true
	case *model.ObjectConsExpression:
		var elems []string
		for _, item := range expr.Items {
			key, ok := keyString(item.Key)
			if !ok {
				return "", false
			}
			v, ok := g.plainPropertyValue(item.Value)
			if !ok {
				return "", false
			}
			elems = append(elems, fmt.Sprintf("(%s.to_string(), %s)", rustString(key), v))
		}
		return "pulumi::PropertyValue::Object(std::collections::BTreeMap::from([" +
			strings.Join(elems, ", ") + "]))", true
	}
	return "", false
}

// ---------------------------------------------------------------------------
// Literals and formatting helpers
// ---------------------------------------------------------------------------

// conversionKind classifies a model type as a runtime conversion target.
func conversionKind(t model.Type) string {
	t = model.ResolveOutputs(t)
	if u, ok := t.(*model.UnionType); ok {
		// Pick the strongest primitive arm for coercion purposes.
		hasNumber, hasInt, hasBool, hasString := false, false, false, false
		for _, e := range u.ElementTypes {
			switch model.ResolveOutputs(e) {
			case model.NumberType:
				hasNumber = true
			case model.IntType:
				hasInt = true
			case model.BoolType:
				hasBool = true
			case model.StringType:
				hasString = true
			}
		}
		switch {
		case hasNumber:
			return "number"
		case hasInt:
			return "int"
		case hasBool:
			return "bool"
		case hasString:
			return "string"
		}
		return ""
	}
	switch t {
	case model.NumberType:
		return "number"
	case model.IntType:
		return "int"
	case model.BoolType:
		return "bool"
	case model.StringType:
		return "string"
	}
	return ""
}

// literalString extracts a static string from literal/template expressions.
// Variable references are NOT literals: a value position referencing a
// variable must not collapse to the variable's name.
func literalString(expr model.Expression) (string, bool) {
	expr = unwrapConvert(expr)
	switch expr := expr.(type) {
	case *model.LiteralValueExpression:
		if expr.Value.Type() == cty.String {
			return expr.Value.AsString(), true
		}
	case *model.TemplateExpression:
		if len(expr.Parts) == 1 {
			return literalString(expr.Parts[0])
		}
	}
	return "", false
}

// keyString extracts a static string in KEY positions, where HCL parses
// bare identifiers as single-part traversals.
func keyString(expr model.Expression) (string, bool) {
	if s, ok := literalString(expr); ok {
		return s, true
	}
	if scope, ok := unwrapConvert(expr).(*model.ScopeTraversalExpression); ok && len(scope.Traversal) == 1 {
		return scope.RootName, true
	}
	return "", false
}

func literalBool(expr model.Expression) (bool, bool) {
	if lit, ok := expr.(*model.LiteralValueExpression); ok && lit.Value.Type() == cty.Bool {
		return lit.Value.True(), true
	}
	return false, false
}

// formatFloat renders a float as a valid Rust f64 literal.
func formatFloat(f float64) string {
	switch {
	case math.IsNaN(f):
		return "f64::NAN"
	case math.IsInf(f, 1):
		return "f64::INFINITY"
	case math.IsInf(f, -1):
		return "f64::NEG_INFINITY"
	}
	s := strconv.FormatFloat(f, 'g', -1, 64)
	if !strings.ContainsAny(s, ".eE") {
		s += ".0"
	}
	return s
}

// rustString renders a Rust string literal with proper escaping.
func rustString(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			b.WriteString("\\\"")
		case '\\':
			b.WriteString("\\\\")
		case '\n':
			b.WriteString("\\n")
		case '\t':
			b.WriteString("\\t")
		case '\r':
			b.WriteString("\\r")
		default:
			if r < 0x20 || r == 0x7f {
				fmt.Fprintf(&b, "\\u{%x}", r)
			} else {
				b.WriteRune(r)
			}
		}
	}
	b.WriteByte('"')
	return b.String()
}
