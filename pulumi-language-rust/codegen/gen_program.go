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
}

func newProgramGenerator(program *pcl.Program) *programGenerator {
	return &programGenerator{
		program:         program,
		functionSchemas: map[string]*schema.Function{},
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
		case *pcl.Component:
			g.errorf(n.Definition.Syntax.DefRange(), "components are not yet supported by the Rust program generator")
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

// ---------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------

func (g *programGenerator) genConfigVariable(w *bytes.Buffer, cfg *pcl.ConfigVariable) {
	key := cfg.LogicalName()
	kind := "object"
	switch cfg.Type() {
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
		def, ok := g.plainPropertyValue(cfg.DefaultValue)
		if !ok {
			g.errorf(cfg.DefaultValue.SyntaxNode().Range(), "unsupported config default value")
			def = "pulumi::PropertyValue::Null"
		}
		expr = fmt.Sprintf("ctx.config().get_%s_or(%q, %s)", kind, key, def)
	} else if cfg.Nullable {
		expr = fmt.Sprintf("ctx.config().get_%s_or(%q, pulumi::PropertyValue::Null)", kind, key)
	} else {
		expr = fmt.Sprintf("ctx.config().require_%s(%q)?", kind, key)
	}
	if cfg.Secret {
		expr = fmt.Sprintf("pulumi::pv::secret(%s)", expr)
	}
	fmt.Fprintf(w, "let %s = %s;\n", varName(cfg.Name()), expr)
}

func (g *programGenerator) genLocalVariable(w *bytes.Buffer, local *pcl.LocalVariable) {
	fmt.Fprintf(w, "let %s = %s;\n", varName(local.Name()), g.expr(local.Definition.Value))
}

func (g *programGenerator) genOutputVariable(w *bytes.Buffer, output *pcl.OutputVariable) {
	fmt.Fprintf(w, "ctx.export(%s, %s);\n", rustString(output.LogicalName()), g.expr(output.Value))
}

// resourcePath returns the Rust path of the generated resource struct plus
// its package name.
func (g *programGenerator) resourcePath(r *pcl.Resource) (structPath, pkgName string) {
	token, _ := r.GetToken()
	parts := strings.Split(token, ":")
	if len(parts) == 2 {
		parts = []string{parts[0], "index", parts[1]}
	}
	pkgName = parts[0]
	module := parts[1]
	member := parts[2]

	if pkgName == "pulumi" && module == "providers" {
		//

		pkgName = member
		return crateName(pkgName) + "::Provider", pkgName
	}

	crate := crateName(pkgName)
	if idx := strings.Index(module, "/"); idx >= 0 {
		module = module[:idx]
	}
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

	structPath, _ := g.resourcePath(r)
	name := varName(r.Name())

	// Build the args struct literal from the schema's input properties.
	var args string
	if r.Schema != nil {
		args = g.typedArgsLiteral(structPath+"Args", r.Schema.InputProperties, r.Inputs, r.Definition.Syntax.DefRange())
	} else {
		g.errorf(r.Definition.Syntax.DefRange(), "resource %q has no schema", r.Name())
		return
	}

	options := g.resourceOptions(r)

	fmt.Fprintf(w, "let %s = %s::new(&ctx, %s, %s, %s);\n",
		name, structPath, rustString(r.LogicalName()), args, options)
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

	var fields []string
	for _, p := range props {
		name := fieldName(p.Name)
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

	// Object-typed inputs become typed args-struct literals.
	if obj, ok := t.(*schema.ObjectType); ok {
		return g.typedObjectLiteral(expr, obj, subject)
	}
	if arr, ok := t.(*schema.ArrayType); ok && containsObject(arr.ElementType) {
		if obj, ok := unwrapToObject(arr.ElementType); ok {
			if tuple, ok := expr.(*model.TupleConsExpression); ok {
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
			if object, ok := expr.(*model.ObjectConsExpression); ok {
				var elems []string
				for _, item := range object.Items {
					key, ok := literalString(item.Key)
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
	object, ok := expr.(*model.ObjectConsExpression)
	if !ok {
		g.errorf(subject, "expected an object literal for type %q", obj.Token)
		return "Default::default()"
	}
	pg := &pkgGenerator{pkg: packageOfObject(obj)}
	argsPath := g.typesPathFor(obj) + pg.typeNameForToken(obj.Token) + "Args"

	var inputs []*model.Attribute
	for _, item := range object.Items {
		key, ok := literalString(item.Key)
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
				key, ok := literalString(item.Key)
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
		}
	}
	if opts.ImportID != nil {
		if s, ok := literalString(opts.ImportID); ok {
			setField("import_id", rustString(s)+".to_string()")
		}
	}
	if opts.CustomTimeouts != nil {
		if object, ok := opts.CustomTimeouts.(*model.ObjectConsExpression); ok {
			var parts []string
			for _, item := range object.Items {
				key, ok1 := literalString(item.Key)
				val, ok2 := literalString(item.Value)
				if ok1 && ok2 {
					parts = append(parts, fmt.Sprintf("%s: %s.to_string()", escapeIdent(key), rustString(val)))
				}
			}
			setField("custom_timeouts", fmt.Sprintf(
				"Some(pulumi::CustomTimeouts { %s, ..Default::default() })", strings.Join(parts, ", ")))
		}
	}

	unsupported := []struct {
		name string
		expr model.Expression
	}{
		{"aliases", opts.Aliases},
		{"providers", opts.Providers},
		{"hideDiffs", opts.HideDiffs},
		{"replaceWith", opts.ReplaceWith},
		{"replacementTrigger", opts.ReplacementTrigger},
		{"envVarMappings", opts.EnvVarMappings},
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
	tuple, ok := expr.(*model.TupleConsExpression)
	if !ok {
		return "", false
	}
	var elems []string
	for _, e := range tuple.Expressions {
		s, ok := literalString(e)
		if !ok {
			return "", false
		}
		elems = append(elems, rustString(s)+".to_string()")
	}
	return "vec![" + strings.Join(elems, ", ") + "]", true
}

// resourceRef resolves an expression referring to a resource variable.
func (g *programGenerator) resourceRef(expr model.Expression) (string, bool) {
	scope, ok := expr.(*model.ScopeTraversalExpression)
	if !ok {
		return "", false
	}
	if len(scope.Parts) > 0 {
		if _, ok := scope.Parts[0].(*pcl.Resource); ok {
			return varName(scope.RootName), true
		}
	}
	return "", false
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
			key, ok := literalString(item.Key)
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
	case *pcl.Resource:
		res := varName(expr.RootName)
		if len(rest) == 0 {
			// A bare resource reference: surface its URN.
			return res + ".urn().cast()"
		}
		attr, ok := rest[0].(hcl.TraverseAttr)
		if !ok {
			g.errorf(expr.SyntaxNode().Range(), "unsupported resource traversal")
			return "pulumi::pv::null()"
		}
		var base string
		switch attr.Name {
		case "urn":
			base = res + ".urn().cast()"
		case "id":
			base = res + ".id().cast()"
		default:
			base = fmt.Sprintf("%s.%s().cast()", res, fieldName(attr.Name))
		}
		_ = root
		return g.traversalChain(base, rest[1:])
	case *pcl.ConfigVariable, *pcl.LocalVariable:
		base := varName(expr.RootName) + ".clone()"
		return g.traversalChain(base, rest)
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
		return g.expr(expr.Args[0])
	case pcl.Invoke:
		return g.invokeExpr(expr)
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
				key, ok := literalString(item.Key)
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
	}
	g.errorf(subject, "function %q is not yet supported by the Rust program generator", expr.Name)
	return "pulumi::pv::null()"
}

// invokeExpr renders a typed invoke call.
func (g *programGenerator) invokeExpr(expr *model.FunctionCallExpression) string {
	subject := expr.SyntaxNode().Range()
	token, ok := literalString(expr.Args[0])
	if !ok {
		g.errorf(subject, "invoke token must be a literal string")
		return "pulumi::pv::null()"
	}
	parts := strings.Split(token, ":")
	if len(parts) == 2 {
		parts = []string{parts[0], "index", parts[1]}
	}
	canonical := strings.Join(parts, ":")
	fn := g.lookupFunction(canonical)
	if fn == nil {
		g.errorf(subject, "unknown function %q", canonical)
		return "pulumi::pv::null()"
	}

	pkgName, module, member := parts[0], parts[1], parts[2]
	if idx := strings.Index(module, "/"); idx >= 0 {
		module = module[:idx]
	}
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
		if object, ok := expr.Args[1].(*model.ObjectConsExpression); ok {
			for _, item := range object.Items {
				key, ok := literalString(item.Key)
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

	return fmt.Sprintf("%s(&ctx, %s, %s).cast()", fnPath, args, options)
}

func (g *programGenerator) invokeOptions(expr model.Expression, subject hcl.Range) string {
	object, ok := expr.(*model.ObjectConsExpression)
	if !ok {
		g.errorf(subject, "invoke options must be an object literal")
		return "pulumi::InvokeOptions::default()"
	}
	var fields []string
	for _, item := range object.Items {
		key, ok := literalString(item.Key)
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
			if tuple, ok := item.Value.(*model.TupleConsExpression); ok {
				var elems []string
				for _, e := range tuple.Expressions {
					if res, ok := g.resourceRef(e); ok {
						elems = append(elems, fmt.Sprintf("%s.pulumi_resource().clone()", res))
					}
				}
				fields = append(fields, "depends_on: vec!["+strings.Join(elems, ", ")+"]")
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
			key, ok := literalString(item.Key)
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

// literalString extracts a static string from literal/template expressions.
func literalString(expr model.Expression) (string, bool) {
	switch expr := expr.(type) {
	case *model.LiteralValueExpression:
		if expr.Value.Type() == cty.String {
			return expr.Value.AsString(), true
		}
	case *model.TemplateExpression:
		if len(expr.Parts) == 1 {
			return literalString(expr.Parts[0])
		}
	case *model.ScopeTraversalExpression:
		// Bare identifiers used as object keys parse as traversals.
		if len(expr.Traversal) == 1 {
			return expr.RootName, true
		}
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
