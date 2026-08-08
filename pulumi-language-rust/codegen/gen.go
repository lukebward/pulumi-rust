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
	"encoding/json"
	"fmt"
	"sort"
	"strings"

	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
)

// GeneratePackage generates a Rust SDK crate for the given schema package.
// The returned map is relative file path -> contents.
func GeneratePackage(
	tool string, pkg *schema.Package,
	extraFiles map[string][]byte, localDependencies map[string]string,
) (map[string][]byte, error) {
	g := &pkgGenerator{
		pkg:          pkg,
		inputTokens:  map[string]*schema.ObjectType{},
		outputTokens: map[string]*schema.ObjectType{},
	}
	g.discoverObjectTypes()

	files := map[string][]byte{}
	files["Cargo.toml"] = g.genCargoToml(localDependencies)
	files["pulumi-plugin.json"] = g.genPulumiPluginJSON()
	files["src/lib.rs"] = g.genLib(tool)
	for p, contents := range extraFiles {
		files[p] = contents
	}
	return files, nil
}

type pkgGenerator struct {
	pkg *schema.Package
	// Object types reachable from resource/function inputs and outputs.
	inputTokens  map[string]*schema.ObjectType
	outputTokens map[string]*schema.ObjectType
}

// version returns the package version string, or a default.
func (g *pkgGenerator) version() string {
	if g.pkg.Version != nil {
		return g.pkg.Version.String()
	}
	return "0.0.1"
}

func (g *pkgGenerator) discoverObjectTypes() {
	var visitInput, visitOutput func(t schema.Type)
	seenIn := map[schema.Type]bool{}
	seenOut := map[schema.Type]bool{}

	visitInput = func(t schema.Type) {
		if t == nil || seenIn[t] {
			return
		}
		seenIn[t] = true
		switch t := t.(type) {
		case *schema.OptionalType:
			visitInput(t.ElementType)
		case *schema.InputType:
			visitInput(t.ElementType)
		case *schema.ArrayType:
			visitInput(t.ElementType)
		case *schema.MapType:
			visitInput(t.ElementType)
		case *schema.ObjectType:
			if t.IsInputShape() {
				t = t.PlainShape
			}
			if g.inputTokens[t.Token] == nil {
				g.inputTokens[t.Token] = t
				for _, p := range t.Properties {
					visitInput(p.Type)
				}
			}
		case *schema.UnionType:
			for _, e := range t.ElementTypes {
				visitInput(e)
			}
		}
	}
	visitOutput = func(t schema.Type) {
		if t == nil || seenOut[t] {
			return
		}
		seenOut[t] = true
		switch t := t.(type) {
		case *schema.OptionalType:
			visitOutput(t.ElementType)
		case *schema.InputType:
			visitOutput(t.ElementType)
		case *schema.ArrayType:
			visitOutput(t.ElementType)
		case *schema.MapType:
			visitOutput(t.ElementType)
		case *schema.ObjectType:
			if t.IsInputShape() {
				t = t.PlainShape
			}
			if g.outputTokens[t.Token] == nil {
				g.outputTokens[t.Token] = t
				for _, p := range t.Properties {
					visitOutput(p.Type)
				}
			}
		case *schema.UnionType:
			for _, e := range t.ElementTypes {
				visitOutput(e)
			}
		}
	}

	resources := g.allResources()
	for _, r := range resources {
		for _, p := range r.InputProperties {
			visitInput(p.Type)
		}
		for _, p := range r.Properties {
			visitOutput(p.Type)
		}
	}
	for _, f := range g.pkg.Functions {
		if f.IsOverlay || f.IsMethod {
			continue
		}
		if f.Inputs != nil {
			for _, p := range f.Inputs.Properties {
				visitInput(p.Type)
			}
		}
		if f.Outputs != nil {
			visitOutput(f.Outputs)
		}
	}
}

func (g *pkgGenerator) allResources() []*schema.Resource {
	var resources []*schema.Resource
	if g.pkg.Provider != nil {
		resources = append(resources, g.pkg.Provider)
	}
	for _, r := range g.pkg.Resources {
		if r.IsOverlay {
			continue
		}
		resources = append(resources, r)
	}
	return resources
}

// typeNameForToken derives the Rust type name for an object type token,
// prefixing the module name for non-index modules to avoid collisions.
func (g *pkgGenerator) typeNameForToken(token string) string {
	mod := g.pkg.TokenToModule(token)
	name := pascalCase(tokenMember(token))
	if mod != "" && mod != "index" {
		name = pascalCase(mod) + name
	}
	return name
}

// containsObject reports whether an object type is reachable in t without
// crossing another object boundary meaning the input form needs Args structs.
func containsObject(t schema.Type) bool {
	switch t := t.(type) {
	case *schema.OptionalType:
		return containsObject(t.ElementType)
	case *schema.InputType:
		return containsObject(t.ElementType)
	case *schema.ArrayType:
		return containsObject(t.ElementType)
	case *schema.MapType:
		return containsObject(t.ElementType)
	case *schema.ObjectType:
		return true
	}
	return false
}

// plainType renders the plain (output-side) Rust type for a schema type.
// qualify is the path prefix for generated types (e.g. "crate::types::").
func (g *pkgGenerator) plainType(t schema.Type, qualify string) string {
	switch t := t.(type) {
	case *schema.OptionalType:
		return "Option<" + g.plainType(t.ElementType, qualify) + ">"
	case *schema.InputType:
		return g.plainType(t.ElementType, qualify)
	case *schema.ArrayType:
		return "std::vec::Vec<" + g.plainType(t.ElementType, qualify) + ">"
	case *schema.MapType:
		return "std::collections::BTreeMap<std::string::String, " + g.plainType(t.ElementType, qualify) + ">"
	case *schema.ObjectType:
		o := t
		if o.IsInputShape() {
			o = o.PlainShape
		}
		return qualify + g.typeNameForToken(o.Token)
	case *schema.EnumType:
		return g.plainType(t.ElementType, qualify)
	case *schema.TokenType:
		if t.UnderlyingType != nil {
			return g.plainType(t.UnderlyingType, qualify)
		}
		return "std::string::String"
	}
	switch t {
	case schema.BoolType:
		return "bool"
	case schema.IntType:
		return "i32"
	case schema.NumberType:
		return "f64"
	case schema.StringType:
		return "std::string::String"
	case schema.AssetType:
		return "pulumi::AssetOrArchive"
	case schema.ArchiveType:
		return "pulumi::Archive"
	}
	// Unions, Any, JSON, resource references, and anything else flow through
	// the dynamic property-value type.
	return "pulumi::PropertyValue"
}

// inputField describes one generated args-struct field.
type inputField struct {
	rustName string
	wireName string
	typ      string // Rust type of the field (without Option wrapper)
	optional bool
	secret   bool
	conv     string // template converting expression %s to Output<PropertyValue>
}

// inputFieldFor computes the Rust representation of an input property.
func (g *pkgGenerator) inputFieldFor(p *schema.Property, qualify string) inputField {
	f := inputField{
		rustName: fieldName(p.Name),
		wireName: p.Name,
		secret:   p.Secret,
	}
	t := p.Type
	if opt, ok := t.(*schema.OptionalType); ok {
		f.optional = true
		t = opt.ElementType
	}
	if in, ok := t.(*schema.InputType); ok {
		t = in.ElementType
	}

	plain := p.Plain
	switch tt := t.(type) {
	case *schema.ObjectType:
		o := tt
		if o.IsInputShape() {
			o = o.PlainShape
		}
		f.typ = qualify + g.typeNameForToken(o.Token) + "Args"
		f.conv = "%s.into_output()"
		return f
	case *schema.ArrayType:
		if containsObject(tt.ElementType) {
			if obj, ok := unwrapToObject(tt.ElementType); ok {
				f.typ = "std::vec::Vec<" + qualify + g.typeNameForToken(obj.Token) + "Args>"
				f.conv = "pulumi::output::all(%s.into_iter().map(|e| e.into_output()).collect()).cast()"
				return f
			}
			// Deeply nested object collections degrade to dynamic values.
			f.typ = "pulumi::Output<pulumi::PropertyValue>"
			f.conv = "%s"
			return f
		}
	case *schema.MapType:
		if containsObject(tt.ElementType) {
			if obj, ok := unwrapToObject(tt.ElementType); ok {
				f.typ = "std::collections::BTreeMap<std::string::String, " + qualify + g.typeNameForToken(obj.Token) + "Args>"
				f.conv = "pulumi::output::object(%s.into_iter().map(|(k, e)| (k, e.into_output())).collect())"
				return f
			}
			f.typ = "pulumi::Output<pulumi::PropertyValue>"
			f.conv = "%s"
			return f
		}
	}

	inner := g.plainType(t, qualify)
	if plain {
		f.typ = inner
		f.conv = "pulumi::Output::from_value(pulumi::IntoPropertyValue::into_property_value(%s))"
	} else {
		f.typ = "pulumi::Output<" + inner + ">"
		f.conv = "%s.cast()"
	}
	return f
}

func unwrapToObject(t schema.Type) (*schema.ObjectType, bool) {
	switch t := t.(type) {
	case *schema.OptionalType:
		return unwrapToObject(t.ElementType)
	case *schema.InputType:
		return unwrapToObject(t.ElementType)
	case *schema.ObjectType:
		o := t
		if o.IsInputShape() {
			o = o.PlainShape
		}
		return o, true
	}
	return nil, false
}

// writeArgsStruct emits an args struct plus its conversion into inputs.
// wrapSecrets marks schema-secret properties as secrets on the wire; this
// applies to resource/function inputs but not nested object types.
func (g *pkgGenerator) writeArgsStruct(
	w *bytes.Buffer, name string, props []*schema.Property, qualify string, wrapSecrets bool,
) {
	fields := make([]inputField, len(props))
	allOptional := true
	for i, p := range props {
		fields[i] = g.inputFieldFor(p, qualify)
		if !fields[i].optional {
			allOptional = false
		}
	}

	if allOptional {
		fmt.Fprintf(w, "#[derive(Clone, Debug, Default)]\n")
	} else {
		fmt.Fprintf(w, "#[derive(Clone, Debug)]\n")
	}
	fmt.Fprintf(w, "pub struct %s {\n", name)
	for _, f := range fields {
		if f.optional {
			fmt.Fprintf(w, "    pub %s: Option<%s>,\n", f.rustName, f.typ)
		} else {
			fmt.Fprintf(w, "    pub %s: %s,\n", f.rustName, f.typ)
		}
	}
	fmt.Fprintf(w, "}\n\n")

	fmt.Fprintf(w, "impl %s {\n", name)
	fmt.Fprintf(w, "    pub fn into_inputs(self) -> std::vec::Vec<(std::string::String, pulumi::Output<pulumi::PropertyValue>)> {\n")
	fmt.Fprintf(w, "        let mut inputs: std::vec::Vec<(std::string::String, pulumi::Output<pulumi::PropertyValue>)> = std::vec::Vec::new();\n")
	for _, f := range fields {
		conv := fmt.Sprintf(f.conv, "v")
		if f.secret && wrapSecrets {
			// Schema-secret properties always marshal as secrets.
			conv = "pulumi::pv::secret(" + conv + ")"
		}
		if f.optional {
			fmt.Fprintf(w, "        if let Some(v) = self.%s {\n", f.rustName)
			fmt.Fprintf(w, "            inputs.push((%q.to_string(), %s));\n", f.wireName, conv)
			fmt.Fprintf(w, "        }\n")
		} else {
			fmt.Fprintf(w, "        {\n")
			fmt.Fprintf(w, "            let v = self.%s;\n", f.rustName)
			fmt.Fprintf(w, "            inputs.push((%q.to_string(), %s));\n", f.wireName, conv)
			fmt.Fprintf(w, "        }\n")
		}
	}
	fmt.Fprintf(w, "        inputs\n")
	fmt.Fprintf(w, "    }\n\n")
	fmt.Fprintf(w, "    pub fn into_output(self) -> pulumi::Output<pulumi::PropertyValue> {\n")
	fmt.Fprintf(w, "        pulumi::output::object(self.into_inputs())\n")
	fmt.Fprintf(w, "    }\n")
	fmt.Fprintf(w, "}\n\n")
}

// writeOutputStruct emits a plain output struct with FromPropertyValue.
func (g *pkgGenerator) writeOutputStruct(
	w *bytes.Buffer, name string, props []*schema.Property, qualify string,
) {
	fmt.Fprintf(w, "#[derive(Clone, Debug)]\n")
	fmt.Fprintf(w, "pub struct %s {\n", name)
	for _, p := range props {
		fmt.Fprintf(w, "    pub %s: %s,\n", fieldName(p.Name), g.plainType(p.Type, qualify))
	}
	fmt.Fprintf(w, "}\n\n")

	fmt.Fprintf(w, "impl pulumi::FromPropertyValue for %s {\n", name)
	fmt.Fprintf(w, "    fn from_property_value(v: pulumi::PropertyValue) -> pulumi::Result<Self> {\n")
	fmt.Fprintf(w, "        let m = <pulumi::PropertyMap as pulumi::FromPropertyValue>::from_property_value(v)?;\n")
	fmt.Fprintf(w, "        Ok(%s {\n", name)
	for _, p := range props {
		fmt.Fprintf(w, "            %s: pulumi::convert::from_property_map(&m, %q)?,\n",
			fieldName(p.Name), p.Name)
	}
	fmt.Fprintf(w, "        })\n")
	fmt.Fprintf(w, "    }\n")
	fmt.Fprintf(w, "}\n\n")
}

// resourceTypeToken returns the registration token for a resource.
func (g *pkgGenerator) resourceTypeToken(r *schema.Resource) string {
	if r.IsProvider {
		return "pulumi:providers:" + g.pkg.Name
	}
	return r.Token
}

// resourceStructName returns the Rust struct name for a resource.
func (g *pkgGenerator) resourceStructName(r *schema.Resource) string {
	if r.IsProvider {
		return "Provider"
	}
	return pascalCase(tokenMember(r.Token))
}

func (g *pkgGenerator) writeResource(w *bytes.Buffer, r *schema.Resource, qualify string) {
	name := g.resourceStructName(r)
	argsName := name + "Args"

	g.writeArgsStruct(w, argsName, r.InputProperties, qualify, true)

	fmt.Fprintf(w, "pub struct %s {\n", name)
	fmt.Fprintf(w, "    resource: pulumi::Resource,\n")
	fmt.Fprintf(w, "}\n\n")

	custom := !r.IsComponent
	remote := r.IsComponent

	var secretOutputs []string
	for _, p := range r.Properties {
		if p.Secret {
			secretOutputs = append(secretOutputs, p.Name)
		}
	}

	fmt.Fprintf(w, "impl %s {\n", name)
	fmt.Fprintf(w, "    pub fn new(ctx: &pulumi::Context, name: &str, args: %s, options: pulumi::ResourceOptions) -> %s {\n", argsName, name)
	if len(secretOutputs) > 0 {
		fmt.Fprintf(w, "        let mut options = options;\n")
		for _, p := range secretOutputs {
			fmt.Fprintf(w, "        options.additional_secret_outputs.push(%q.to_string());\n", p)
		}
	}
	fmt.Fprintf(w, "        let resource = ctx.register_resource(pulumi::RegisterRequest {\n")
	fmt.Fprintf(w, "            type_: %q.to_string(),\n", g.resourceTypeToken(r))
	fmt.Fprintf(w, "            name: name.to_string(),\n")
	fmt.Fprintf(w, "            custom: %v,\n", custom)
	fmt.Fprintf(w, "            remote: %v,\n", remote)
	fmt.Fprintf(w, "            version: %q.to_string(),\n", g.version())
	fmt.Fprintf(w, "            plugin_download_url: %q.to_string(),\n", g.pkg.PluginDownloadURL)
	fmt.Fprintf(w, "            inputs: args.into_inputs(),\n")
	fmt.Fprintf(w, "            options,\n")
	fmt.Fprintf(w, "        });\n")
	fmt.Fprintf(w, "        %s { resource }\n", name)
	fmt.Fprintf(w, "    }\n\n")
	fmt.Fprintf(w, "    pub fn pulumi_resource(&self) -> &pulumi::Resource {\n")
	fmt.Fprintf(w, "        &self.resource\n")
	fmt.Fprintf(w, "    }\n\n")
	fmt.Fprintf(w, "    pub fn urn(&self) -> pulumi::Output<std::string::String> {\n")
	fmt.Fprintf(w, "        self.resource.urn()\n")
	fmt.Fprintf(w, "    }\n")
	if custom {
		fmt.Fprintf(w, "\n    pub fn id(&self) -> pulumi::Output<std::string::String> {\n")
		fmt.Fprintf(w, "        self.resource.id()\n")
		fmt.Fprintf(w, "    }\n")
	}

	// Output property accessors. Schema-secret properties are always
	// surfaced as secrets, even while the value is unknown.
	for _, p := range r.Properties {
		accessor := fieldName(p.Name)
		typ := g.plainType(p.Type, qualify)
		fmt.Fprintf(w, "\n    pub fn %s(&self) -> pulumi::Output<%s> {\n", accessor, typ)
		if p.Secret {
			fmt.Fprintf(w, "        self.resource.output(%q).as_secret().cast()\n", p.Name)
		} else {
			fmt.Fprintf(w, "        self.resource.output(%q).cast()\n", p.Name)
		}
		fmt.Fprintf(w, "    }\n")
	}
	fmt.Fprintf(w, "}\n\n")
}

func (g *pkgGenerator) writeFunction(w *bytes.Buffer, f *schema.Function, qualify string) {
	name := pascalCase(tokenMember(f.Token))
	fnName := functionName(tokenMember(f.Token))
	argsName := name + "Args"

	var props []*schema.Property
	if f.Inputs != nil {
		props = f.Inputs.Properties
	}
	g.writeArgsStruct(w, argsName, props, qualify, true)

	resultType := "pulumi::PropertyValue"
	if f.Outputs != nil {
		o := f.Outputs
		if o.IsInputShape() {
			o = o.PlainShape
		}
		resultType = qualify + g.typeNameForToken(o.Token)
	}

	fmt.Fprintf(w, "pub fn %s(ctx: &pulumi::Context, args: %s, options: pulumi::InvokeOptions) -> pulumi::Output<%s> {\n",
		fnName, argsName, resultType)
	fmt.Fprintf(w, "    let mut options = options;\n")
	fmt.Fprintf(w, "    if options.version.is_empty() {\n")
	fmt.Fprintf(w, "        options.version = %q.to_string();\n", g.version())
	fmt.Fprintf(w, "    }\n")
	if g.pkg.PluginDownloadURL != "" {
		fmt.Fprintf(w, "    if options.plugin_download_url.is_empty() {\n")
		fmt.Fprintf(w, "        options.plugin_download_url = %q.to_string();\n", g.pkg.PluginDownloadURL)
		fmt.Fprintf(w, "    }\n")
	}
	fmt.Fprintf(w, "    ctx.invoke(%q, args.into_inputs(), options).cast()\n", f.Token)
	fmt.Fprintf(w, "}\n\n")
}

func (g *pkgGenerator) genLib(tool string) []byte {
	var w bytes.Buffer
	fmt.Fprintf(&w, "// Code generated by %s. DO NOT EDIT.\n", tool)
	fmt.Fprintf(&w, "#![allow(unused_imports, unused_variables, unused_mut, dead_code, clippy::all)]\n\n")

	// Group resources and functions by module.
	type module struct {
		resources []*schema.Resource
		functions []*schema.Function
	}
	modules := map[string]*module{}
	getModule := func(name string) *module {
		if name == "index" {
			name = ""
		}
		m := modules[name]
		if m == nil {
			m = &module{}
			modules[name] = m
		}
		return m
	}

	if g.pkg.Provider != nil {
		getModule("").resources = append(getModule("").resources, g.pkg.Provider)
	}
	for _, r := range g.pkg.Resources {
		if r.IsOverlay {
			continue
		}
		getModule(g.pkg.TokenToModule(r.Token)).resources = append(
			getModule(g.pkg.TokenToModule(r.Token)).resources, r)
	}
	for _, f := range g.pkg.Functions {
		if f.IsOverlay || f.IsMethod {
			continue
		}
		getModule(g.pkg.TokenToModule(f.Token)).functions = append(
			getModule(g.pkg.TokenToModule(f.Token)).functions, f)
	}

	modNames := make([]string, 0, len(modules))
	for name := range modules {
		modNames = append(modNames, name)
	}
	sort.Strings(modNames)

	for _, modName := range modNames {
		m := modules[modName]
		sort.Slice(m.resources, func(i, j int) bool {
			return g.resourceTypeToken(m.resources[i]) < g.resourceTypeToken(m.resources[j])
		})
		sort.Slice(m.functions, func(i, j int) bool { return m.functions[i].Token < m.functions[j].Token })

		qualify := "crate::types::"
		var body bytes.Buffer
		for _, r := range m.resources {
			g.writeResource(&body, r, qualify)
		}
		for _, f := range m.functions {
			g.writeFunction(&body, f, qualify)
		}

		if modName == "" {
			w.Write(body.Bytes())
		} else {
			fmt.Fprintf(&w, "pub mod %s {\n", modIdent(modName))
			for _, line := range strings.Split(strings.TrimRight(body.String(), "\n"), "\n") {
				if line == "" {
					w.WriteString("\n")
				} else {
					fmt.Fprintf(&w, "    %s\n", line)
				}
			}
			fmt.Fprintf(&w, "}\n\n")
		}
	}

	// The types module: args structs for input objects, plain structs for
	// output objects.
	inputNames := make([]string, 0, len(g.inputTokens))
	for token := range g.inputTokens {
		inputNames = append(inputNames, token)
	}
	sort.Strings(inputNames)
	outputNames := make([]string, 0, len(g.outputTokens))
	for token := range g.outputTokens {
		outputNames = append(outputNames, token)
	}
	sort.Strings(outputNames)

	if len(inputNames) > 0 || len(outputNames) > 0 {
		var body bytes.Buffer
		qualify := "crate::types::"
		for _, token := range inputNames {
			t := g.inputTokens[token]
			g.writeArgsStruct(&body, g.typeNameForToken(token)+"Args", t.Properties, qualify, false)
		}
		for _, token := range outputNames {
			t := g.outputTokens[token]
			g.writeOutputStruct(&body, g.typeNameForToken(token), t.Properties, qualify)
		}
		fmt.Fprintf(&w, "pub mod types {\n")
		for _, line := range strings.Split(strings.TrimRight(body.String(), "\n"), "\n") {
			if line == "" {
				w.WriteString("\n")
			} else {
				fmt.Fprintf(&w, "    %s\n", line)
			}
		}
		fmt.Fprintf(&w, "}\n")
	}

	return w.Bytes()
}

func (g *pkgGenerator) genCargoToml(localDependencies map[string]string) []byte {
	var w bytes.Buffer
	fmt.Fprintf(&w, "[package]\n")
	fmt.Fprintf(&w, "name = %q\n", crateName(g.pkg.Name))
	fmt.Fprintf(&w, "version = %q\n", g.version())
	fmt.Fprintf(&w, "edition = \"2021\"\n\n")
	fmt.Fprintf(&w, "[dependencies]\n")
	if path, ok := localDependencies["pulumi"]; ok {
		fmt.Fprintf(&w, "pulumi = { path = %q }\n", path)
	} else {
		fmt.Fprintf(&w, "pulumi = \"0.1\"\n")
	}

	// Reference sibling local packages this schema depends on.
	depNames := make([]string, 0, len(localDependencies))
	for name := range localDependencies {
		if name != "pulumi" && name != g.pkg.Name && g.schemaReferencesPackage(name) {
			depNames = append(depNames, name)
		}
	}
	sort.Strings(depNames)
	for _, name := range depNames {
		fmt.Fprintf(&w, "%s = { path = %q }\n", crateName(name), localDependencies[name])
	}

	fmt.Fprintf(&w, "\n[workspace]\n")
	return w.Bytes()
}

// schemaReferencesPackage reports whether the schema declares a dependency on
// another package by name.
func (g *pkgGenerator) schemaReferencesPackage(name string) bool {
	for _, dep := range g.pkg.Dependencies {
		if dep.Name == name {
			return true
		}
	}
	return false
}

func (g *pkgGenerator) genPulumiPluginJSON() []byte {
	type pluginJSON struct {
		Resource bool   `json:"resource"`
		Name     string `json:"name"`
		Version  string `json:"version"`
		Server   string `json:"server,omitempty"`
	}
	data, err := json.MarshalIndent(pluginJSON{
		Resource: true,
		Name:     g.pkg.Name,
		Version:  g.version(),
		Server:   g.pkg.PluginDownloadURL,
	}, "", "  ")
	if err != nil {
		panic(err)
	}
	return append(data, '\n')
}
