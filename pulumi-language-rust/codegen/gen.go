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
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"sort"
	"strconv"
	"strings"

	"github.com/pulumi/pulumi/pkg/v3/codegen/schema"
)

// GeneratePackage generates a Rust SDK crate for the given schema package.
// The returned map is relative file path -> contents.
func GeneratePackage(
	tool string, pkg *schema.Package,
	extraFiles map[string][]byte, localDependencies map[string]string,
) (map[string][]byte, error) {
	g := newPkgGenerator(pkg)
	g.findRecursiveTypes()

	files := map[string][]byte{}
	files["Cargo.toml"] = g.genCargoToml(localDependencies)
	files["pulumi-plugin.json"] = g.genPulumiPluginJSON()
	files["src/lib.rs"] = g.genLib(tool)
	if len(g.errs) > 0 {
		return nil, fmt.Errorf("generating the %s SDK: %w", pkg.Name, errors.Join(g.errs...))
	}
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
	// Tokens whose object type is a function's synthesized result rather
	// than a type the schema declares. The binder mints these by appending
	// "Result" to the function's member name, which can land on the token
	// of a declared type; resolveTypeNames uses this to decide which of the
	// two keeps the undecorated name.
	functionResults map[string]bool
	// Object token -> Rust type name, assigned once by resolveTypeNames so
	// that distinct tokens never share a name. Read through typeNameForToken.
	typeNames map[string]string
	// Object tokens that sit on a containment cycle, grouped by cycle.
	// See findRecursiveTypes.
	cycleGroup map[string]int
	// Failures collected while emitting. The writers return no error of
	// their own, so they record here and GeneratePackage reports the lot
	// rather than handing back a crate that quietly does the wrong thing.
	errs []error
	// Set when some property defaults to an environment variable, so the
	// helpers that read one are emitted only into the crates that use them.
	needsEnvHelpers bool
}

// newPkgGenerator returns a generator whose object types are discovered and
// whose type names are resolved — the state every emitter reads. Callers
// that only want to *name* a type in an SDK someone else generates want one
// of these too, since a name depends on the whole token set.
func newPkgGenerator(pkg *schema.Package) *pkgGenerator {
	g := &pkgGenerator{
		pkg:             pkg,
		inputTokens:     map[string]*schema.ObjectType{},
		outputTokens:    map[string]*schema.ObjectType{},
		functionResults: map[string]bool{},
		typeNames:       map[string]string{},
	}
	g.discoverObjectTypes()
	g.resolveTypeNames()
	return g
}

// fail records a generation failure. Emitting continues so one run reports
// every problem rather than the first.
func (g *pkgGenerator) fail(err error) {
	g.errs = append(g.errs, err)
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
			o := f.Outputs
			if o.IsInputShape() {
				o = o.PlainShape
			}
			g.functionResults[o.Token] = true
		}
	}
}

// directObject unwraps the wrappers that do not introduce a heap
// allocation in the generated Rust — an optional or an input marker — and
// reports the object type underneath, if any. `Vec` and `BTreeMap` are
// deliberately not unwrapped: they are pointers to a separate allocation,
// so a type reached through one does not contribute to its container's
// size.
func directObject(t schema.Type) (*schema.ObjectType, bool) {
	switch t := t.(type) {
	case *schema.OptionalType:
		return directObject(t.ElementType)
	case *schema.InputType:
		return directObject(t.ElementType)
	case *schema.ObjectType:
		o := t
		if o.IsInputShape() {
			o = o.PlainShape
		}
		return o, true
	}
	return nil, false
}

// findRecursiveTypes locates object types that contain themselves — the
// Kubernetes schema's `JSONSchemaProps.not`, for instance, whose type is
// `JSONSchemaProps`. A Rust struct with such a field has no finite size, so
// the field has to be boxed. Boxing every edge that lies on a cycle breaks
// all of them, so the question a field asks is "does my type belong to the
// same cycle as the struct declaring me?" — which is exactly "are we in the
// same strongly connected component of the direct-containment graph".
//
// Populates g.cycleGroup with a component id per token, and only for
// components that actually contain a cycle: a single object type that never
// refers to itself is its own trivial component and is left out, so nothing
// gets boxed unnecessarily.
func (g *pkgGenerator) findRecursiveTypes() {
	g.cycleGroup = map[string]int{}

	// The direct-containment graph over every object type the SDK emits.
	// Input and output shapes generate two Rust structs from the same
	// schema properties, so one graph covers both.
	objects := map[string]*schema.ObjectType{}
	for token, t := range g.inputTokens {
		objects[token] = t
	}
	for token, t := range g.outputTokens {
		objects[token] = t
	}
	tokens := make([]string, 0, len(objects))
	for token := range objects {
		tokens = append(tokens, token)
	}
	sort.Strings(tokens)

	edges := map[string][]string{}
	for _, token := range tokens {
		for _, p := range objects[token].Properties {
			if o, ok := directObject(p.Type); ok {
				if _, known := objects[o.Token]; known {
					edges[token] = append(edges[token], o.Token)
				}
			}
		}
	}

	// Tarjan's algorithm, iterating tokens in sorted order so component ids
	// are stable across runs.
	type state struct {
		index, lowlink int
		onStack        bool
	}
	states := map[string]*state{}
	var stack []string
	next := 1
	group := 1

	var strongConnect func(v string)
	strongConnect = func(v string) {
		s := &state{index: next, lowlink: next, onStack: true}
		states[v] = s
		next++
		stack = append(stack, v)

		for _, w := range edges[v] {
			if ws, seen := states[w]; !seen {
				strongConnect(w)
				if states[w].lowlink < s.lowlink {
					s.lowlink = states[w].lowlink
				}
			} else if ws.onStack && ws.index < s.lowlink {
				s.lowlink = ws.index
			}
		}

		if s.lowlink != s.index {
			return
		}
		var component []string
		for {
			w := stack[len(stack)-1]
			stack = stack[:len(stack)-1]
			states[w].onStack = false
			component = append(component, w)
			if w == v {
				break
			}
		}
		// A one-token component is only a cycle if the token refers to
		// itself; larger ones always are.
		if len(component) == 1 {
			selfRef := false
			for _, w := range edges[v] {
				if w == v {
					selfRef = true
					break
				}
			}
			if !selfRef {
				return
			}
		}
		for _, w := range component {
			g.cycleGroup[w] = group
		}
		group++
	}

	for _, token := range tokens {
		if _, seen := states[token]; !seen {
			strongConnect(token)
		}
	}
}

// closesCycle reports whether a field of type t, declared on the object
// type `owner`, would make owner's generated struct infinitely sized. Such
// a field is emitted boxed. An empty owner — a resource or function args
// struct, which no object type can refer back to — never needs boxing.
func (g *pkgGenerator) closesCycle(owner string, t schema.Type) bool {
	if owner == "" {
		return false
	}
	o, ok := directObject(t)
	if !ok {
		return false
	}
	ownerGroup, ok := g.cycleGroup[owner]
	if !ok {
		return false
	}
	return g.cycleGroup[o.Token] == ownerGroup
}

func (g *pkgGenerator) allResources() []*schema.Resource {
	resources := make([]*schema.Resource, 0, len(g.pkg.Resources)+1)
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

// deriveTypeName is the naming rule: the token's member, prefixed with the
// module name for non-index modules. It is not injective — a token carries
// more than its module and member, so two distinct tokens can derive the
// same name. resolveTypeNames is what makes the result unique.
func (g *pkgGenerator) deriveTypeName(token string) string {
	mod := g.pkg.TokenToModule(token)
	name := pascalCase(tokenMember(token))
	if mod != "" && mod != "index" {
		name = pascalCase(mod) + name
	}
	return name
}

// resolveTypeNames assigns every discovered object token a Rust type name
// that no other token holds. Both forms of a name are reserved together,
// since a token emits `Name` on the output side and `NameArgs` on the input
// side and either one may be the thing that clashes.
//
// Tokens the schema declares are named first, function results second, so
// that a synthesized `<function>Result` token yields to a declared type of
// the same name rather than the other way round: a declared type can be
// referenced from anywhere in the schema, while a function result is only
// ever named by its own invoke. Go's generator resolves the same collision
// in the same order — it registers types before functions — but repairs it
// by renaming the function, which a Rust generator has no reason to do
// since a function and a struct do not share a namespace here.
func (g *pkgGenerator) resolveTypeNames() {
	declared := make([]string, 0, len(g.inputTokens)+len(g.outputTokens))
	results := make([]string, 0, len(g.functionResults))
	seen := map[string]bool{}
	for _, tokens := range []map[string]*schema.ObjectType{g.inputTokens, g.outputTokens} {
		for token := range tokens {
			if seen[token] {
				continue
			}
			seen[token] = true
			if g.functionResults[token] {
				results = append(results, token)
			} else {
				declared = append(declared, token)
			}
		}
	}
	sort.Strings(declared)
	sort.Strings(results)

	taken := map[string]bool{}
	assign := func(token string) {
		base := g.deriveTypeName(token)
		name := base
		for n := 2; taken[name] || taken[name+"Args"]; n++ {
			name = base + strconv.Itoa(n)
		}
		taken[name], taken[name+"Args"] = true, true
		g.typeNames[token] = name
	}
	for _, token := range declared {
		assign(token)
	}
	for _, token := range results {
		assign(token)
	}
}

// typeNameForToken reports the Rust type name for an object type token.
func (g *pkgGenerator) typeNameForToken(token string) string {
	if name, ok := g.typeNames[token]; ok {
		return name
	}
	return g.deriveTypeName(token)
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

// plainFieldType renders the type of one field of a plain output struct.
// It is plainType, except that a field closing a containment cycle is
// boxed — inside the Option, since it is the field's own size that has to
// become finite.
func (g *pkgGenerator) plainFieldType(owner string, t schema.Type, qualify string) string {
	inner := t
	optional := false
	if o, ok := inner.(*schema.OptionalType); ok {
		optional = true
		inner = o.ElementType
	}
	rendered := g.plainType(inner, qualify)
	if g.closesCycle(owner, inner) {
		rendered = "std::boxed::Box<" + rendered + ">"
	}
	if optional {
		rendered = "Option<" + rendered + ">"
	}
	return rendered
}

// inputField describes one generated args-struct field.
type inputField struct {
	rustName string
	wireName string
	typ      string // Rust type of the field (without the Option wrapper)
	// required records what the SCHEMA says, not how the field is declared:
	// every generated field is an Option so the struct can derive Default and
	// callers can elide what they do not set. Required-ness is reported at
	// registration time instead, the way the Go, C#, Java and Python SDKs do.
	required bool
	secret   bool
	conv     string // template converting expression %s to Output<PropertyValue>
	// defaultValue, when non-empty, is a PropertyValue expression used when
	// the input is unset (schema default).
	defaultValue string
	// defaultEnv, when non-empty, is an expression yielding
	// Option<PropertyValue> read from the environment. It takes precedence
	// over defaultValue, matching every other Pulumi SDK.
	defaultEnv string
}

// fieldNamesFor assigns each property a unique Rust field name. Distinct
// wire names can fold to the same snake_case identifier; later ones get
// underscore suffixes. Deterministic given the property order, so the SDK
// generator and program generator agree.
func fieldNamesFor(props []*schema.Property) map[string]string {
	names := map[string]string{}
	used := map[string]bool{}
	for _, p := range props {
		candidate := fieldName(p.Name)
		for used[candidate] {
			candidate += "_"
		}
		names[p.Name] = candidate
		used[candidate] = true
	}
	return names
}

// isResourceRef reports whether a schema type is a reference to a resource,
// looking through the optional and input wrappers.
func isResourceRef(t schema.Type) bool {
	switch t := t.(type) {
	case *schema.OptionalType:
		return isResourceRef(t.ElementType)
	case *schema.InputType:
		return isResourceRef(t.ElementType)
	case *schema.ResourceType:
		return true
	}
	return false
}

// inputFieldFor computes the Rust representation of an input property.
// owner is the object-type token the field is declared on, or "" for a
// resource or function args struct; it decides whether the field has to be
// boxed to keep the struct finitely sized.
func (g *pkgGenerator) inputFieldFor(owner string, p *schema.Property, qualify string) inputField {
	f := inputField{
		rustName: fieldName(p.Name),
		wireName: p.Name,
		secret:   p.Secret,
	}
	t := p.Type
	if opt, ok := t.(*schema.OptionalType); ok {
		t = opt.ElementType
	} else {
		f.required = true
	}
	if in, ok := t.(*schema.InputType); ok {
		t = in.ElementType
	}

	if dv := p.DefaultValue; dv != nil {
		if dv.Value != nil {
			value, err := plainConstValue(dv.Value)
			if err != nil {
				where := fmt.Sprintf("input %q", p.Name)
				if owner != "" {
					where = fmt.Sprintf("%s of %s", where, owner)
				}
				g.fail(fmt.Errorf("%s: %w", where, err))
			}
			f.defaultValue = value
		}
		f.defaultEnv = g.envDefaultExpr(t, dv.Environment)
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
		if g.closesCycle(owner, tt) {
			f.typ = "std::boxed::Box<" + f.typ + ">"
			f.conv = "(*%s).into_output()"
		}
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

// requiredWireNames renders the schema-required input names as the elements
// of a Rust static slice.
func requiredWireNames(props []*schema.Property) string {
	var names []string
	for _, p := range props {
		if p.IsRequired() {
			names = append(names, fmt.Sprintf("%q", p.Name))
		}
	}
	return strings.Join(names, ", ")
}

// writeArgsStruct emits an args struct plus its conversion into inputs.
// wrapSecrets marks schema-secret properties as secrets on the wire; this
// applies to resource/function inputs but not nested object types.
func (g *pkgGenerator) writeArgsStruct(
	w *bytes.Buffer, owner, name string, props []*schema.Property, qualify string, wrapSecrets bool,
) {
	names := fieldNamesFor(props)
	fields := make([]inputField, len(props))
	for i, p := range props {
		fields[i] = g.inputFieldFor(owner, p, qualify)
		fields[i].rustName = names[p.Name]
	}

	// Every field is an Option and every struct derives Default, so a caller
	// names only the inputs it sets and elides the rest with
	// `..Default::default()`. Rust struct literals have no way to omit a
	// field, so declaring required inputs as bare types — the obvious
	// encoding — forced a caller to write out all 44 fields of a struct like
	// Azure's WebAppArgs to set one. Required-ness is reported when the
	// resource registers instead, which is where Go, C#, Java and Python
	// report it too.
	fmt.Fprintf(w, "#[derive(Clone, Debug, Default)]\n")
	fmt.Fprintf(w, "pub struct %s {\n", name)
	for _, f := range fields {
		fmt.Fprintf(w, "    pub %s: Option<%s>,\n", f.rustName, f.typ)
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
		// Wrap a PropertyValue expression the way an explicitly supplied
		// value would be wrapped, so a default travels as the same shape.
		asInput := func(value string) string {
			out := fmt.Sprintf("pulumi::Output::from_value(%s)", value)
			if f.secret && wrapSecrets {
				out = "pulumi::pv::secret(" + out + ")"
			}
			return out
		}
		fmt.Fprintf(w, "        if let Some(v) = self.%s {\n", f.rustName)
		fmt.Fprintf(w, "            inputs.push((%q.to_string(), %s));\n", f.wireName, conv)
		// The environment is consulted before the static default: an
		// operator who exports AWS_REGION means it to win over the value
		// baked into the schema.
		if f.defaultEnv != "" {
			fmt.Fprintf(w, "        } else if let Some(v) = %s {\n", f.defaultEnv)
			fmt.Fprintf(w, "            inputs.push((%q.to_string(), %s));\n", f.wireName, asInput("v"))
		}
		if f.defaultValue != "" {
			fmt.Fprintf(w, "        } else {\n")
			fmt.Fprintf(w, "            inputs.push((%q.to_string(), %s));\n", f.wireName, asInput(f.defaultValue))
		}
		fmt.Fprintf(w, "        }\n")
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
	w *bytes.Buffer, owner, name string, props []*schema.Property, qualify string,
) {
	names := fieldNamesFor(props)
	fmt.Fprintf(w, "#[derive(Clone, Debug)]\n")
	fmt.Fprintf(w, "pub struct %s {\n", name)
	for _, p := range props {
		fmt.Fprintf(w, "    pub %s: %s,\n", names[p.Name], g.plainFieldType(owner, p.Type, qualify))
	}
	fmt.Fprintf(w, "}\n\n")

	fmt.Fprintf(w, "impl pulumi::FromPropertyValue for %s {\n", name)
	fmt.Fprintf(w, "    fn from_property_value(v: pulumi::PropertyValue) -> pulumi::Result<Self> {\n")
	fmt.Fprintf(w, "        let m = <pulumi::PropertyMap as pulumi::FromPropertyValue>::from_property_value(v)?;\n")
	fmt.Fprintf(w, "        Ok(%s {\n", name)
	for _, p := range props {
		fmt.Fprintf(w, "            %s: pulumi::convert::from_property_map(&m, %q)?,\n",
			names[p.Name], p.Name)
	}
	fmt.Fprintf(w, "        })\n")
	fmt.Fprintf(w, "    }\n")
	fmt.Fprintf(w, "}\n\n")
}

// envHelpersModule is emitted into a crate whose schema declares an
// environment-variable default. It mirrors the equivalent helpers in the Go,
// Python and Node SDKs: probe the named variables in order and take the
// first one that is set, so `AWS_REGION` fills in a provider's `region`
// without the program repeating it.
const envHelpersModule = `#[doc(hidden)]
pub mod internal {
    /// The value of the first of names that is set, if any. A variable set
    /// to the empty string counts as set, matching the other SDKs.
    pub fn env_var(names: &[&str]) -> Option<std::string::String> {
        names.iter().find_map(|name| std::env::var(name).ok())
    }

    pub fn env_string(names: &[&str]) -> Option<pulumi::PropertyValue> {
        env_var(names).map(pulumi::PropertyValue::String)
    }

    pub fn env_bool(names: &[&str]) -> Option<pulumi::PropertyValue> {
        // The spellings Go's strconv.ParseBool accepts, which is what the
        // other SDKs settled on.
        match env_var(names)?.as_str() {
            "1" | "t" | "T" | "true" | "TRUE" | "True" => Some(pulumi::PropertyValue::Bool(true)),
            "0" | "f" | "F" | "false" | "FALSE" | "False" => Some(pulumi::PropertyValue::Bool(false)),
            _ => None,
        }
    }

    pub fn env_int(names: &[&str]) -> Option<pulumi::PropertyValue> {
        let parsed = env_var(names)?.parse::<i64>().ok()?;
        Some(pulumi::PropertyValue::Number(parsed as f64))
    }

    pub fn env_number(names: &[&str]) -> Option<pulumi::PropertyValue> {
        let parsed = env_var(names)?.parse::<f64>().ok()?;
        Some(pulumi::PropertyValue::Number(parsed))
    }
}
`

// envDefaultExpr renders the Option<PropertyValue> expression that reads a
// property's default out of the environment, or "" when the property has no
// environment default or has a type no environment variable can carry — a
// string is the only thing the process environment holds, so an object or a
// collection has nothing sensible to decode into and is left alone.
func (g *pkgGenerator) envDefaultExpr(t schema.Type, environment []string) string {
	if len(environment) == 0 {
		return ""
	}
	var helper string
	switch underlyingPrimitive(t) {
	case schema.BoolType:
		helper = "env_bool"
	case schema.IntType:
		helper = "env_int"
	case schema.NumberType:
		helper = "env_number"
	case schema.StringType:
		helper = "env_string"
	default:
		return ""
	}
	names := make([]string, len(environment))
	for i, name := range environment {
		names[i] = strconv.Quote(name)
	}
	g.needsEnvHelpers = true
	return fmt.Sprintf("crate::internal::%s(&[%s])", helper, strings.Join(names, ", "))
}

// underlyingPrimitive looks through the wrappers that do not change what a
// value is on the wire and reports the underlying primitive type, or nil.
func underlyingPrimitive(t schema.Type) schema.Type {
	switch t := t.(type) {
	case *schema.OptionalType:
		return underlyingPrimitive(t.ElementType)
	case *schema.InputType:
		return underlyingPrimitive(t.ElementType)
	case *schema.EnumType:
		return underlyingPrimitive(t.ElementType)
	case *schema.UnionType:
		// A union's default is bound against whichever member accepts it;
		// the default type is the one the schema nominates.
		if t.DefaultType != nil {
			return underlyingPrimitive(t.DefaultType)
		}
		return nil
	}
	switch t {
	case schema.BoolType, schema.IntType, schema.NumberType, schema.StringType:
		return t
	}
	return nil
}

// plainConstValue renders a schema constant as a PropertyValue expression.
//
// An unrecognised constant is an error rather than an empty string: the
// caller reads "no expression" as "this property has no default", so a shape
// that falls off the end of this switch would drop a declared default on the
// floor and leave the provider to fail at runtime over a value the schema
// said was always there.
func plainConstValue(v any) (string, error) {
	number := func(literal string) string {
		return fmt.Sprintf("pulumi::PropertyValue::Number(%s)", literal)
	}
	integer := func(i int64) string {
		// Not routed through formatFloat: past 2^53 that would round, and
		// silently shipping a different number is what this function exists
		// to avoid.
		return number(strconv.FormatInt(i, 10) + ".0")
	}
	switch v := v.(type) {
	case bool:
		return fmt.Sprintf("pulumi::PropertyValue::Bool(%v)", v), nil
	case int:
		return integer(int64(v)), nil
	case int32:
		return integer(int64(v)), nil
	case int64:
		return integer(v), nil
	case float32:
		return number(formatFloat(float64(v))), nil
	case float64:
		return number(formatFloat(v)), nil
	case json.Number:
		// A schema decoded with json.Decoder.UseNumber keeps its numbers as
		// text rather than as float64.
		f, err := v.Float64()
		if err != nil {
			return "", fmt.Errorf("default value %s is not a number: %w", v.String(), err)
		}
		return number(formatFloat(f)), nil
	case string:
		return fmt.Sprintf("pulumi::PropertyValue::String(%s.to_string())", rustString(v)), nil
	case []any:
		elements := make([]string, len(v))
		for i, e := range v {
			rendered, err := plainConstValue(e)
			if err != nil {
				return "", fmt.Errorf("in element %d: %w", i, err)
			}
			elements[i] = rendered
		}
		return fmt.Sprintf("pulumi::PropertyValue::Array(vec![%s])", strings.Join(elements, ", ")), nil
	case map[string]any:
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		entries := make([]string, len(keys))
		for i, k := range keys {
			rendered, err := plainConstValue(v[k])
			if err != nil {
				return "", fmt.Errorf("in entry %q: %w", k, err)
			}
			entries[i] = fmt.Sprintf("(%s.to_string(), %s)", rustString(k), rendered)
		}
		return fmt.Sprintf(
			"pulumi::PropertyValue::Object(std::collections::BTreeMap::from([%s]))",
			strings.Join(entries, ", ")), nil
	}
	return "", fmt.Errorf("cannot render a default value of type %T", v)
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

	g.writeArgsStruct(w, "", argsName, r.InputProperties, qualify, true)

	fmt.Fprintf(w, "#[derive(Clone)]\n")
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
	var replaceOnChanges []string
	for _, p := range r.InputProperties {
		if p.ReplaceOnChanges {
			replaceOnChanges = append(replaceOnChanges, p.Name)
		}
	}

	fmt.Fprintf(w, "impl %s {\n", name)
	fmt.Fprintf(w, "    pub fn new(ctx: &pulumi::Context, name: &str, args: %s, options: pulumi::ResourceOptions) -> %s {\n", argsName, name)
	if len(secretOutputs) > 0 || len(replaceOnChanges) > 0 {
		fmt.Fprintf(w, "        let mut options = options;\n")
		for _, p := range secretOutputs {
			fmt.Fprintf(w, "        options.additional_secret_outputs.push(%q.to_string());\n", p)
		}
		for _, p := range replaceOnChanges {
			fmt.Fprintf(w, "        options.replace_on_changes.push(%q.to_string());\n", p)
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
	fmt.Fprintf(w, "            package: %s,\n", g.packageDescriptorExpr())
	fmt.Fprintf(w, "            deferred_inputs: vec![],\n")
	// Every args field is an Option so the struct can derive Default, so the
	// schema's required inputs are checked when the resource registers.
	// Wire names, because that is what the schema and the error message use.
	fmt.Fprintf(w, "            required: &[%s],\n", requiredWireNames(r.InputProperties))
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
	// The same naming the program generator uses to *call* these accessors.
	// The rule lived in both files and had to be kept in step by hand; a
	// resource with an output named `id` made this emit `id_()` while the
	// program emitted `id()`, which does not compile.
	accessorNames := outputAccessorNames(r.Properties)
	for _, p := range r.Properties {
		accessor := accessorNames[p.Name]
		typ := g.plainType(p.Type, qualify)
		// A resource-typed output is a reference the engine must hydrate,
		// even when the program only forwards it onward.
		hydrate := ""
		if isResourceRef(p.Type) {
			hydrate = ".hydrated()"
		}
		fmt.Fprintf(w, "\n    pub fn %s(&self) -> pulumi::Output<%s> {\n", accessor, typ)
		if p.Secret {
			fmt.Fprintf(w, "        self.resource.output(%q).as_secret()%s.cast()\n", p.Name, hydrate)
		} else {
			fmt.Fprintf(w, "        self.resource.output(%q)%s.cast()\n", p.Name, hydrate)
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
	g.writeArgsStruct(w, "", argsName, props, qualify, true)

	resultType := "pulumi::PropertyValue"
	scalarReturn := false
	if f.Outputs != nil {
		o := f.Outputs
		if o.IsInputShape() {
			o = o.PlainShape
		}
		resultType = qualify + g.typeNameForToken(o.Token)
	} else if f.ReturnType != nil {
		// Non-object returns arrive as a single-property object that the
		// SDK unwraps to the scalar.
		scalarReturn = true
	}

	fmt.Fprintf(w, "pub fn %s(ctx: &pulumi::Context, args: %s, options: pulumi::InvokeOptions) -> pulumi::Output<%s> {\n",
		fnName, argsName, resultType)
	fmt.Fprintf(w, "    let mut options = options;\n")
	fmt.Fprintf(w, "    if options.version.is_empty() {\n")
	fmt.Fprintf(w, "        options.version = %q.to_string();\n", g.version())
	fmt.Fprintf(w, "    }\n")
	if desc := g.packageDescriptorExpr(); desc != "None" {
		fmt.Fprintf(w, "    if options.package.is_none() {\n")
		fmt.Fprintf(w, "        options.package = %s;\n", desc)
		fmt.Fprintf(w, "    }\n")
	}
	if g.pkg.PluginDownloadURL != "" {
		fmt.Fprintf(w, "    if options.plugin_download_url.is_empty() {\n")
		fmt.Fprintf(w, "        options.plugin_download_url = %q.to_string();\n", g.pkg.PluginDownloadURL)
		fmt.Fprintf(w, "    }\n")
	}
	if scalarReturn {
		fmt.Fprintf(w, "    pulumi::pv::single_value(ctx.invoke(%q, args.into_inputs(), options)).cast()\n", f.Token)
	} else {
		fmt.Fprintf(w, "    ctx.invoke(%q, args.into_inputs(), options).cast()\n", f.Token)
	}
	fmt.Fprintf(w, "}\n\n")
}

// packageDescriptorExpr renders the parameterized package this crate serves,
// or None for an ordinary package. Registrations carry it so the engine can
// resolve the base plugin plus its parameter.
func (g *pkgGenerator) packageDescriptorExpr() string {
	var baseName, baseVersion string
	var parameter []byte
	extension := false
	if p := g.pkg.Parameterization; p != nil {
		baseName, baseVersion, parameter = p.BasePlugin.Name, p.BasePlugin.Version.String(), p.Parameter
	} else if p := g.pkg.ExtensionParameterization; p != nil {
		baseName, baseVersion, parameter = p.BaseProvider.Name, p.BaseProvider.Version.String(), p.Parameter
		extension = true
	} else {
		return "None"
	}
	return fmt.Sprintf("Some(pulumi::PackageDescriptor { base_name: %q.to_string(), base_version: %q.to_string(), download_url: %q.to_string(), name: %q.to_string(), version: %q.to_string(), base64_parameter: %q.to_string(), extension: %v })",
		baseName, baseVersion, g.pkg.PluginDownloadURL, g.pkg.Name, g.version(),
		base64.StdEncoding.EncodeToString(parameter), extension)
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
			g.writeArgsStruct(&body, token, g.typeNameForToken(token)+"Args", t.Properties, qualify, false)
		}
		for _, token := range outputNames {
			t := g.outputTokens[token]
			g.writeOutputStruct(&body, token, g.typeNameForToken(token), t.Properties, qualify)
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

	// Emitted last, and only when something above referenced it, so a crate
	// with no environment defaults is byte-for-byte what it was before.
	if g.needsEnvHelpers {
		fmt.Fprintf(&w, "\n%s", envHelpersModule)
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
	type parameterizationJSON struct {
		Name    string `json:"name"`
		Version string `json:"version"`
		Value   []byte `json:"value"`
	}
	type pluginJSON struct {
		Resource                  bool                  `json:"resource"`
		Name                      string                `json:"name"`
		Version                   string                `json:"version"`
		Server                    string                `json:"server,omitempty"`
		Parameterization          *parameterizationJSON `json:"parameterization,omitempty"`
		ExtensionParameterization *parameterizationJSON `json:"extensionParameterization,omitempty"`
	}
	// A parameterized package installs its BASE plugin; the parameter tells
	// that plugin which package to serve.
	out := pluginJSON{
		Resource: true,
		Name:     g.pkg.Name,
		Version:  g.version(),
		Server:   g.pkg.PluginDownloadURL,
	}
	if p := g.pkg.Parameterization; p != nil {
		out.Name = p.BasePlugin.Name
		out.Version = p.BasePlugin.Version.String()
		out.Parameterization = &parameterizationJSON{
			Name:    g.pkg.Name,
			Version: g.version(),
			Value:   p.Parameter,
		}
	} else if p := g.pkg.ExtensionParameterization; p != nil {
		out.Name = p.BaseProvider.Name
		out.Version = p.BaseProvider.Version.String()
		out.ExtensionParameterization = &parameterizationJSON{
			Name:    g.pkg.Name,
			Version: g.version(),
			Value:   p.Parameter,
		}
	}
	data, err := json.MarshalIndent(out, "", "  ")
	if err != nil {
		panic(err)
	}
	return append(data, '\n')
}
