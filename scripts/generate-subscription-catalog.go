//go:build ignore

// Generate the Mihomo field catalog from the pinned upstream Go definitions.
// Usage: go run scripts/generate-subscription-catalog.go /path/to/mihomo-v1.19.31 > mihomo.schema.json
package main

import (
	"encoding/json"
	"go/ast"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"
)

type node struct {
	expr    ast.Expr
	imports map[string]string
}

var types = map[string]node{}
var textTypes = map[string]bool{}

var literalConstants = map[string]node{}
var constantText = map[string]string{}

type enumMapping struct {
	pkg  string
	node node
}

var enumMappings = map[string]enumMapping{}
var enumValues = map[string][]string{}
var enumFolded = map[string]bool{}

func setEnum(field map[string]any, choices []string, folded bool) {
	if folded {
		patterns := []string{}
		for _, choice := range choices {
			var b strings.Builder
			for _, c := range choice {
				if unicode.IsLetter(c) {
					b.WriteString("[" + string(unicode.ToLower(c)) + string(unicode.ToUpper(c)) + "]")
				} else {
					b.WriteString(regexp.QuoteMeta(string(c)))
				}
			}
			patterns = append(patterns, b.String())
		}
		delete(field, "enum")
		field["pattern"] = "^(?:" + strings.Join(patterns, "|") + ")$"
		field["x-enum-choices"] = choices
	} else {
		field["enum"] = choices
	}
}

var factoryDefaults node
var factoryPackage string

func qualified(expr ast.Expr, pkg string, imports map[string]string) string {
	switch t := expr.(type) {
	case *ast.Ident:
		return pkg + "." + t.Name
	case *ast.SelectorExpr:
		if p, ok := t.X.(*ast.Ident); ok {
			if path := imports[p.Name]; path != "" {
				return path + "." + t.Sel.Name
			}
			return ""
		}
	}
	return ""
}
func tagName(field *ast.Field) string {
	if field.Tag == nil {
		return ""
	}
	value, _ := strconv.Unquote(field.Tag.Value)
	for _, key := range []string{"yaml", "mapstructure", "group", "provider", "inbound"} {
		if name := strings.Split(reflect.StructTag(value).Get(key), ",")[0]; name != "" {
			return name
		}
	}
	return ""
}
func resolvedType(expr ast.Expr, pkg string, imports map[string]string, depth int) (ast.Expr, string, map[string]string) {
	if depth > 20 {
		return expr, pkg, imports
	}
	key := qualified(expr, pkg, imports)
	if n, ok := types[key]; ok {
		pkg = key[:strings.LastIndex(key, ".")]
		return resolvedType(n.expr, pkg, n.imports, depth+1)
	}
	return expr, pkg, imports
}
func defaultValue(expr ast.Expr, pkg string, imports map[string]string, depth int) (any, bool) {
	if depth > 30 {
		return nil, false
	}
	if text, ok := constantText[qualified(expr, pkg, imports)]; ok && qualified(expr, pkg, imports) != "" {
		return text, true
	}
	switch t := expr.(type) {
	case *ast.Ident:
		if t.Name == "true" {
			return true, true
		}
		if t.Name == "false" {
			return false, true
		}
	case *ast.BasicLit:
		if t.Kind == token.STRING {
			value, err := strconv.Unquote(t.Value)
			return value, err == nil
		}
		if t.Kind == token.INT {
			value, err := strconv.ParseInt(t.Value, 0, 64)
			return value, err == nil
		}
		if t.Kind == token.FLOAT {
			value, err := strconv.ParseFloat(t.Value, 64)
			return value, err == nil
		}
	case *ast.UnaryExpr:
		if t.Op == token.AND {
			return defaultValue(t.X, pkg, imports, depth+1)
		}
	case *ast.BinaryExpr:
		if t.Op == token.ADD {
			a, aok := defaultValue(t.X, pkg, imports, depth+1)
			b, bok := defaultValue(t.Y, pkg, imports, depth+1)
			aa, as := a.(string)
			bb, bs := b.(string)
			if aok && bok && as && bs {
				return aa + bb, true
			}
		}
	case *ast.CallExpr:
		if selector, ok := t.Fun.(*ast.SelectorExpr); ok {
			if selector.Sel.Name == "ToLower" && len(t.Args) == 1 {
				if value, ok := defaultValue(t.Args[0], pkg, imports, depth+1); ok {
					if text, ok := value.(string); ok {
						return strings.ToLower(text), true
					}
				}
			}
			if selector.Sel.Name == "String" {
				if value, ok := constantText[qualified(selector.X, pkg, imports)]; ok {
					return value, true
				}
			}
			if (selector.Sel.Name == "MustParsePrefix" || selector.Sel.Name == "MustParseAddr") && len(t.Args) == 1 {
				return defaultValue(t.Args[0], pkg, imports, depth+1)
			}
		}
	case *ast.CompositeLit:
		typ, typePackage, typeImports := resolvedType(t.Type, pkg, imports, 0)
		if _, ok := typ.(*ast.ArrayType); ok {
			values := []any{}
			for _, item := range t.Elts {
				value, ok := defaultValue(item, pkg, imports, depth+1)
				if !ok {
					return nil, false
				}
				values = append(values, value)
			}
			return values, true
		}
		if _, ok := typ.(*ast.MapType); ok && len(t.Elts) == 0 {
			return map[string]any{}, true
		}
		if structure, ok := typ.(*ast.StructType); ok {
			names := map[string]string{}
			for _, field := range structure.Fields.List {
				for _, name := range field.Names {
					names[name.Name] = tagName(field)
				}
			}
			values := map[string]any{}
			for _, item := range t.Elts {
				if pair, ok := item.(*ast.KeyValueExpr); ok {
					if key, ok := pair.Key.(*ast.Ident); ok && names[key.Name] != "" {
						if value, ok := defaultValue(pair.Value, typePackage, typeImports, depth+1); ok {
							values[names[key.Name]] = value
						}
					}
				}
			}
			return values, true
		}
	}
	key := qualified(expr, pkg, imports)
	if value, ok := literalConstants[key]; ok {
		return defaultValue(value.expr, key[:strings.LastIndex(key, ".")], value.imports, depth+1)
	}
	return nil, false
}
func defaultsForSchema(definition map[string]any, value any) any {
	if values, ok := value.(map[string]any); ok {
		properties, _ := definition["properties"].(map[string]any)
		result := map[string]any{}
		for key, value := range values {
			if field, ok := properties[key].(map[string]any); ok {
				result[key] = defaultsForSchema(field, value)
			} else if field, ok := definition["additionalProperties"].(map[string]any); ok {
				result[key] = defaultsForSchema(field, value)
			}
		}
		return result
	}
	return value
}
func attachDefaults(definition map[string]any, value any) {
	definition["default"] = defaultsForSchema(definition, value)
	definition["x-default-source"] = "Mihomo v1.19.31 DefaultRawConfig"
	if values, ok := value.(map[string]any); ok {
		properties, _ := definition["properties"].(map[string]any)
		for key, value := range values {
			if field, ok := properties[key].(map[string]any); ok {
				attachDefaults(field, value)
			}
		}
	}
}
func removeNonpublic(value any) {
	switch v := value.(type) {
	case map[string]any:
		delete(v, "processors-per-channel")
		for _, child := range v {
			removeNonpublic(child)
		}
	case []any:
		for _, child := range v {
			removeNonpublic(child)
		}
	}
}
func annotateDefaults(definition map[string]any) {
	definition["x-default-behavior"] = "native_when_unset"
	if _, ok := definition["default"]; ok {
		definition["x-default-behavior"] = "literal"
		if _, ok := definition["x-default-source"]; !ok {
			definition["x-default-source"] = "Mihomo v1.19.31 parser defaults"
		}
	}
	definition["x-validation-version"] = "1.19.31"
	for _, key := range []string{"items", "additionalProperties"} {
		if child, ok := definition[key].(map[string]any); ok {
			annotateDefaults(child)
		}
	}
	if properties, ok := definition["properties"].(map[string]any); ok {
		for _, field := range properties {
			if field, ok := field.(map[string]any); ok {
				annotateDefaults(field)
			}
		}
	}
	for _, key := range []string{"oneOf", "anyOf", "allOf"} {
		if values, ok := definition[key].([]any); ok {
			for _, field := range values {
				if field, ok := field.(map[string]any); ok {
					annotateDefaults(field)
				}
			}
		}
	}
}

func schema(expr ast.Expr, pkg string, imports map[string]string, seen map[string]bool) map[string]any {
	s := map[string]any{}
	switch t := expr.(type) {
	case *ast.StarExpr:
		return schema(t.X, pkg, imports, seen)
	case *ast.Ident:
		switch t.Name {
		case "string":
			s["type"] = "string"
		case "bool":
			s["type"] = "boolean"
		case "int", "int8", "int16", "int32", "int64", "uint", "uint8", "uint16", "uint32", "uint64":
			s["type"] = "integer"
			if strings.HasPrefix(t.Name, "uint") {
				s["minimum"] = 0
			}
		case "float32", "float64":
			s["type"] = "number"
		case "any":
			s["x-unverified"] = true
		default:
			key := pkg + "." + t.Name
			if textTypes[key] {
				field := map[string]any{"type": "string"}
				if choices := enumValues[key]; len(choices) > 0 {
					setEnum(field, choices, enumFolded[key])
				}
				return field
			}
			if v, ok := types[key]; ok && !seen[key] {
				next := map[string]bool{}
				for k, v := range seen {
					next[k] = v
				}
				next[key] = true
				return schema(v.expr, pkg, v.imports, next)
			}
			s["type"] = "string"
			s["x-unverified"] = true
		}
	case *ast.SelectorExpr:
		if p, ok := t.X.(*ast.Ident); ok {
			target := imports[p.Name]
			if target == "net/netip" {
				return map[string]any{"type": "string"}
			}
			if target != "" {
				return schema(ast.NewIdent(t.Sel.Name), target, nil, seen)
			}
		}
		s["type"] = "string"
		s["x-unverified"] = true
	case *ast.ArrayType:
		s["type"] = "array"
		s["items"] = schema(t.Elt, pkg, imports, seen)
	case *ast.MapType:
		s["type"] = "object"
		s["additionalProperties"] = schema(t.Value, pkg, imports, seen)
	case *ast.StructType:
		s["type"] = "object"
		props := map[string]any{}
		s["properties"] = props
		s["additionalProperties"] = false
		for _, f := range t.Fields.List {
			if len(f.Names) == 0 {
				child := schema(f.Type, pkg, imports, seen)
				if p, ok := child["properties"].(map[string]any); ok {
					for k, v := range p {
						props[k] = v
					}
				}
				continue
			}
			if f.Tag == nil {
				continue
			}
			tag, _ := strconv.Unquote(f.Tag.Value)
			name := strings.Split(reflect.StructTag(tag).Get("yaml"), ",")[0]
			if name == "" {
				name = strings.Split(reflect.StructTag(tag).Get("mapstructure"), ",")[0]
			}
			if name == "" {
				name = strings.Split(reflect.StructTag(tag).Get("group"), ",")[0]
			}
			if name == "" {
				name = strings.Split(reflect.StructTag(tag).Get("provider"), ",")[0]
			}
			if name == "" {
				name = strings.Split(reflect.StructTag(tag).Get("inbound"), ",")[0]
			}
			if name == "" {
				name = strings.Split(reflect.StructTag(tag).Get("proxy"), ",")[0]
			}
			if name == "" || name == "-" {
				continue
			}
			props[name] = schema(f.Type, pkg, imports, seen)
		}
	default:
		s["x-unverified"] = true
	}
	return s
}
func main() {
	root := os.Args[1]
	module := "github.com/metacubex/mihomo"
	filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
		if err != nil || d.IsDir() || !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return nil
		}
		f, e := parser.ParseFile(token.NewFileSet(), path, nil, 0)
		if e != nil {
			return nil
		}
		rel, _ := filepath.Rel(root, filepath.Dir(path))
		pkg := module
		if rel != "." {
			pkg += "/" + rel
		}
		imports := map[string]string{}
		for _, im := range f.Imports {
			p, _ := strconv.Unquote(im.Path.Value)
			p = strings.ReplaceAll(p, "MetaCubeX", "metacubex")
			alias := filepath.Base(p)
			if im.Name != nil {
				alias = im.Name.Name
			}
			imports[alias] = p
		}
		for _, decl := range f.Decls {
			if fn, ok := decl.(*ast.FuncDecl); ok {
				if fn.Recv != nil {
					receiver := fn.Recv.List[0].Type
					if ptr, ok := receiver.(*ast.StarExpr); ok {
						receiver = ptr.X
					}
					key := qualified(receiver, pkg, imports)
					ast.Inspect(fn.Body, func(n ast.Node) bool {
						if call, ok := n.(*ast.CallExpr); ok {
							if selector, ok := call.Fun.(*ast.SelectorExpr); ok && selector.Sel.Name == "ToLower" {
								enumFolded[key] = true
							}
						}
						return true
					})
				}

				if fn.Name.Name == "String" && fn.Recv != nil {
					ast.Inspect(fn.Body, func(n ast.Node) bool {
						if clause, ok := n.(*ast.CaseClause); ok && len(clause.List) > 0 {
							for _, statement := range clause.Body {
								if ret, ok := statement.(*ast.ReturnStmt); ok && len(ret.Results) == 1 {
									if literal, ok := ret.Results[0].(*ast.BasicLit); ok && literal.Kind == token.STRING {
										value, _ := strconv.Unquote(literal.Value)
										for _, constant := range clause.List {
											if key := qualified(constant, pkg, imports); key != "" {
												constantText[key] = value
											}
										}
									}
								}
							}
						}
						return true
					})
				}
				if pkg == module+"/config" && fn.Name.Name == "DefaultRawConfig" {
					for _, statement := range fn.Body.List {
						if ret, ok := statement.(*ast.ReturnStmt); ok && len(ret.Results) > 0 {
							factoryDefaults = node{ret.Results[0], imports}
							factoryPackage = pkg
						}
					}
				}
			}
			if fn, ok := decl.(*ast.FuncDecl); ok && fn.Name.Name == "MarshalText" && fn.Recv != nil {
				r := fn.Recv.List[0].Type
				if ptr, ok := r.(*ast.StarExpr); ok {
					r = ptr.X
				}
				if id, ok := r.(*ast.Ident); ok {
					textTypes[pkg+"."+id.Name] = true
				}
			}
			if g, ok := decl.(*ast.GenDecl); ok {
				for _, spec := range g.Specs {
					if g.Tok == token.VAR {
						if value, ok := spec.(*ast.ValueSpec); ok {
							for _, expr := range value.Values {
								if literal, ok := expr.(*ast.CompositeLit); ok {
									if mapping, ok := literal.Type.(*ast.MapType); ok {
										enumMappings[qualified(mapping.Value, pkg, imports)] = enumMapping{pkg, node{literal, imports}}
									}
								}
							}
						}
					}

					if g.Tok == token.CONST {
						if value, ok := spec.(*ast.ValueSpec); ok {
							for i, name := range value.Names {
								if i < len(value.Values) {
									literalConstants[pkg+"."+name.Name] = node{value.Values[i], imports}
								}
							}
						}
					}
					if t, ok := spec.(*ast.TypeSpec); ok {
						types[pkg+"."+t.Name.Name] = node{t.Type, imports}
					}
				}
			}
		}
		return nil
	})
	for key, mapping := range enumMappings {
		choices := []string{}
		valid := true
		for _, item := range mapping.node.expr.(*ast.CompositeLit).Elts {
			pair, ok := item.(*ast.KeyValueExpr)
			if !ok {
				valid = false
				break
			}
			value, ok := defaultValue(pair.Key, mapping.pkg, mapping.node.imports, 0)
			text, isString := value.(string)
			if !ok || !isString {
				valid = false
				break
			}
			choices = append(choices, text)
		}
		if valid && len(choices) > 0 {
			sort.Strings(choices)
			enumValues[key] = choices
		}
	}
	result := schema(ast.NewIdent("RawConfig"), module+"/config", nil, map[string]bool{})
	result["$schema"] = "https://json-schema.org/draft/2020-12/schema"
	result["x-source"] = "https://github.com/MetaCubeX/mihomo/tree/v1.19.31"
	ruleTypes := []string{}
	if rulesFile, err := parser.ParseFile(token.NewFileSet(), filepath.Join(root, "rules/parser.go"), nil, 0); err == nil {
		ast.Inspect(rulesFile, func(n ast.Node) bool {
			if clause, ok := n.(*ast.CaseClause); ok {
				for _, expr := range clause.List {
					if literal, ok := expr.(*ast.BasicLit); ok && literal.Kind == token.STRING {
						if value, err := strconv.Unquote(literal.Value); err == nil {
							ruleTypes = append(ruleTypes, value)
						}
					}
				}
			}
			return true
		})
	}
	sort.Strings(ruleTypes)
	result["x-rule-types"] = ruleTypes
	// map-based policy objects are parsed dynamically upstream; supply their typed option structs.
	props := result["properties"].(map[string]any)
	group := schema(ast.NewIdent("GroupCommonOption"), module+"/adapter/outboundgroup", nil, map[string]bool{})
	if p, ok := group["properties"].(map[string]any); ok {
		for _, name := range []string{"GroupBaseOption", "SelectorOption", "URLTestOption", "LoadBalanceOption"} {
			v := schema(ast.NewIdent(name), module+"/adapter/outboundgroup", nil, map[string]bool{})
			if q, ok := v["properties"].(map[string]any); ok {
				for k, v := range q {
					p[k] = v
				}
			}
		}
		p["name"] = map[string]any{"type": "string"}
		p["type"] = map[string]any{"type": "string", "enum": []string{"select", "url-test", "fallback", "load-balance", "relay"}}
		p["proxies"] = map[string]any{"type": "array", "items": map[string]any{"type": "string"}}
	}
	if fields, ok := group["properties"].(map[string]any); ok {
		for _, key := range []string{"proxies", "empty-fallback"} {
			if field, ok := fields[key].(map[string]any); ok {
				if items, ok := field["items"].(map[string]any); ok {
					items["x-tag-reference"] = "outbound"
				} else {
					field["x-tag-reference"] = "outbound"
				}
			}
		}
	}
	props["proxy-groups"] = map[string]any{"type": "array", "items": group}

	props["rule-providers"] = map[string]any{"type": "object", "additionalProperties": schema(ast.NewIdent("ruleProviderSchema"), module+"/rules/provider", nil, map[string]bool{})}
	if provider, ok := props["rule-providers"].(map[string]any)["additionalProperties"].(map[string]any); ok {
		if fields, ok := provider["properties"].(map[string]any); ok {
			if field, ok := fields["proxy"].(map[string]any); ok {
				field["x-tag-reference"] = "outbound"
			}
		}
	}
	props["proxy-providers"] = map[string]any{"type": "object", "maxProperties": 0, "additionalProperties": false, "description": "Nodes are supplied by authorized dynamic injection"}
	// Dynamic nodes use the same pinned option structs as the native proxy parser.
	proxy := schema(ast.NewIdent("Hysteria2Option"), module+"/adapter/outbound", nil, map[string]bool{})
	proxyFields := proxy["properties"].(map[string]any)
	proxyFields["type"] = map[string]any{"type": "string", "const": "hysteria2"}
	proxyFields["udp"] = map[string]any{"type": "boolean"}
	proxyFields["port"] = map[string]any{"type": "integer", "minimum": 1, "maximum": 65535}
	// Mihomo's weakly typed proxy decoder accepts numeric hop intervals in seconds.
	proxyFields["hop-interval"] = map[string]any{"anyOf": []any{
		map[string]any{"type": "string"}, map[string]any{"type": "integer", "minimum": 0},
	}}
	proxy["required"] = []string{"name", "type", "server", "password"}
	props["proxies"] = map[string]any{"type": "array", "items": proxy, "description": "Read-only dynamic Hysteria2 nodes"}
	dns := props["dns"].(map[string]any)["properties"].(map[string]any)
	listable := map[string]any{"anyOf": []any{map[string]any{"type": "string"}, map[string]any{"type": "array", "items": map[string]any{"type": "string"}}}}
	for _, key := range []string{"nameserver-policy", "proxy-server-nameserver-policy"} {
		dns[key] = map[string]any{"type": "object", "additionalProperties": listable}
	}
	props["hosts"] = map[string]any{"type": "object", "additionalProperties": listable}
	inboundVariants := []any{}
	inboundTypes := map[string]string{"http": "HTTPOption", "socks": "SocksOption", "mixed": "MixedOption", "redir": "RedirOption", "tproxy": "TProxyOption", "tun": "TunOption", "tunnel": "TunnelOption"}
	inboundKinds := []string{}
	for kind := range inboundTypes {
		inboundKinds = append(inboundKinds, kind)
	}
	sort.Strings(inboundKinds)
	for _, kind := range inboundKinds {
		typ := inboundTypes[kind]
		v := schema(ast.NewIdent(typ), module+"/listener/inbound", nil, map[string]bool{})
		if p, ok := v["properties"].(map[string]any); ok {
			p["type"] = map[string]any{"const": kind}
			if field, ok := p["proxy"].(map[string]any); ok {
				field["x-tag-reference"] = "outbound"
			}
			if field, ok := p["rule"].(map[string]any); ok {
				field["x-tag-reference"] = "sub_rule"
			}
			v["required"] = []string{"type"}
			inboundVariants = append(inboundVariants, v)
		}
	}
	props["listeners"] = map[string]any{"type": "array", "items": map[string]any{"oneOf": inboundVariants}}
	for key, values := range map[string][]string{"mode": {"rule", "global", "direct"}, "log-level": {"debug", "info", "warning", "error", "silent"}, "find-process-mode": {"always", "strict", "off"}} {
		field := props[key].(map[string]any)
		if _, hasChoices := field["x-enum-choices"]; !hasChoices {
			setEnum(field, values, true)
		}
	}
	// Native rules are ordered strings; the upstream JSON tag is singular but YAML uses rules.
	delete(props, "rule")
	for _, key := range []string{"port", "socks-port", "redir-port", "tproxy-port", "mixed-port"} {
		if field, ok := props[key].(map[string]any); ok {
			field["minimum"] = 0
			field["maximum"] = 65535
		}
	}
	for _, variant := range inboundVariants {
		if fields, ok := variant.(map[string]any)["properties"].(map[string]any); ok {
			if _, ok := fields["port"]; ok {
				fields["port"] = map[string]any{"anyOf": []any{
					map[string]any{"type": "string"},
					map[string]any{"type": "integer", "minimum": 0, "maximum": 65535},
				}}
			}
		}
	}

	// Public defaults come from the pinned factory initializer; build-dependent expressions remain unset.
	removeNonpublic(result)
	delete(props["tun"].(map[string]any)["properties"].(map[string]any), "processors-per-channel")
	if factoryDefaults.expr != nil {
		if value, ok := defaultValue(factoryDefaults.expr, factoryPackage, factoryDefaults.imports, 0); ok {
			attachDefaults(result, value)
		}
	}

	if tun, ok := props["tun"].(map[string]any); ok {
		fields := tun["properties"].(map[string]any)
		delete(fields, "processors-per-channel")

		if value, ok := tun["default"].(map[string]any); ok {
			delete(value, "processors-per-channel")
		}
	}
	provider := props["rule-providers"].(map[string]any)["additionalProperties"].(map[string]any)
	providerFields := provider["properties"].(map[string]any)
	providerFields["type"].(map[string]any)["enum"] = []string{"http", "file", "inline"}
	providerFields["behavior"].(map[string]any)["enum"] = []string{"domain", "ipcidr", "classical"}
	providerFields["format"].(map[string]any)["enum"] = []string{"", "yaml", "text", "mrs"}
	providerFields["format"].(map[string]any)["default"] = "yaml"
	groupFields := group["properties"].(map[string]any)
	groupFields["lazy"].(map[string]any)["default"] = true
	groupFields["empty-fallback"].(map[string]any)["default"] = "COMPATIBLE"
	annotateDefaults(result)
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(result); err != nil {
		panic(err)
	}
}
