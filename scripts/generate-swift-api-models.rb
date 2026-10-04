#!/usr/bin/env ruby
# Generate macOS request/response DTOs and API operation definitions from OpenAPI.

require "optparse"
require "yaml"

root = File.expand_path("..", __dir__)
spec_path = File.join(root, "openapi/openapi.yaml")
output_path = File.join(root, "apps/macos/Sources/HysteriaX/Models/OpenAPIRequests.generated.swift")
response_output_path = File.join(root, "apps/macos/Sources/HysteriaX/Models/OpenAPIResponses.generated.swift")
check_only = false
OptionParser.new do |options|
  options.on("--check", "fail if the checked-in Swift models are out of date") do
    check_only = true
  end
end.parse!

spec = YAML.load_file(spec_path)
schemas = spec.fetch("components").fetch("schemas")
models = {
  "NodeUsageUpdate" => "NodeUsageUpdateRequest",
  "NodeCreate" => "NodeCreateRequest",
  "NodePatch" => "NodePatchRequest",
  "ResourceCreate" => "ResourceUploadRequest",
  "UserCreate" => "UserCreateRequest",
  "UserPatch" => "UserPatchRequest",
  "Revision" => "RevisionRequest",
  "AssignmentRequest" => "AssignmentRequest",
  "AssignmentCertificateUpdate" => "AssignmentCertificateUpdateRequest",
}
response_models = %w[
  NodePackage NodePackageUsage NodeAlert NodeUsageUpdateResponse
  NodeSummary NodeSSHDetail NodeConnectionDetail NodeDetail
  AssignmentInfo UserSummary JobOutcome JobResult JobSummary AuditSummary
  NodeResource ResourceReceipt AssignmentReceipt SubscriptionReceipt
  AssignmentMutationResponse
  RotatedCredential RotatedCredentials JobReceipt
  NodeUpdateResponse UserUpdateResponse QuotaResetResponse
  ActiveSubscription SubscriptionStatus NodeUsageSummary DataFreshness PendingRevocation
  UserUsageResponse JobDetailResponse HysteriaAuthResponse
  CreatedEntity NodeReceipt APIHealth APIVersion APIErrorDetail APIErrorResponse
  AdminTokenSummary AdminTokenReceipt ServerMonitoring
  OverviewIssue OverviewNode OverviewResponse OverviewBucket OverviewHistory
]
identifiable_models = %w[NodeSummary NodeDetail UserSummary JobSummary AuditSummary NodeResource AdminTokenSummary]

def resolve_schema(schema, schemas, stack = [])
  if schema.key?("$ref")
    name = schema.fetch("$ref").split("/").last
    raise "recursive schema reference: #{name}" if stack.include?(name)
    return resolve_schema(schemas.fetch(name), schemas, stack + [name])
  end

  if schema.key?("allOf")
    parts = schema.fetch("allOf").map { |part| resolve_schema(part, schemas, stack) }
    return {
      "properties" => parts.each_with_object({}) { |part, values| values.merge!(part.fetch("properties", {})) },
      "required" => parts.flat_map { |part| part.fetch("required", []) }.uniq,
    }
  end

  schema
end

def swift_type(schema, schemas)
  if schema.key?("$ref")
    return schema.fetch("$ref").split("/").last
  end
  type = schema["type"]
  if type.is_a?(Array)
    type = type.reject { |entry| entry == "null" }.first
  end

  case type
  when "string" then "String"
  when "integer" then schema["format"] == "int64" ? "Int64" : "Int"
  when "number" then "Double"
  when "boolean" then "Bool"
  when "object" then "[String: JSONValue]"
  when "array" then "[#{swift_type(schema.fetch("items", {"type" => "string"}), schemas)}]"
  else
    raise "unsupported OpenAPI type: #{type.inspect} in #{schema.inspect}"
  end
end

def swift_name(name, response = false)
  overrides = {
    "tls_sni" => "tlsSNI",
    "tls_skip_verify" => "tlsSkipVerify",
    "node_id" => "nodeID",
  }
  if response
    overrides.merge!(
      "entity_id" => "entityID",
      "content_sha256" => "contentSHA256",
      "api_version" => "apiVersion",
      "service_version" => "serviceVersion",
      "hysteria_version" => "hysteriaVersion",
      "mihomo_version" => "mihomoVersion",
      "node_auth_token" => "nodeAuthToken",
      "hy2_credential" => "hy2Credential",
      "server_url" => "serverURL",
      "yaml_preview" => "yamlPreview",
      "listen_addr" => "listenAddress",
      "skip_cert_verify" => "skipCertVerify",
      "public" => "connection",
    )
  end
  return overrides.fetch(name) if overrides.key?(name)

  name.split("_").each_with_index.map { |part, index| index.zero? ? part : part[0].upcase + part[1..] }.join
end

def swift_literal(value, type)
  case value
  when true then "true"
  when false then "false"
  when Hash then "[:]"
  when String then value.dump
  when Numeric then value.to_s
  else
    raise "unsupported default value #{value.inspect} for #{type}"
  end
end

def request_fields(schema, schemas, schema_name)
  properties = schema.fetch("properties", {})
  required = schema.fetch("required", [])
  properties.map do |field_name, field_schema|
    name = swift_name(field_name)
    type = swift_type(field_schema, schemas)
    nullable = Array(field_schema["type"]).include?("null")
    # PATCH null means "clear this value", while an omitted key means "leave it
    # unchanged". PatchValue preserves that distinction without losing the schema type.
    type = "PatchValue<#{type}>" if schema_name == "UserPatch" && nullable
    is_required = required.include?(field_name)
    default_present = field_schema.key?("default")
    optional = nullable || (!is_required && !default_present)
    default = if default_present
      swift_literal(field_schema["default"], type)
    elsif optional
      "nil"
    end
    { json: field_name, swift: name, type: optional ? "#{type}?" : type, default: default }
  end
end

def render_request_model(schema_name, swift_struct, schema, schemas)
  fields = request_fields(schema, schemas, schema_name)
  generated_lines = ["struct #{swift_struct}: Encodable, Sendable {"]
  fields.each { |field| generated_lines << "    let #{field[:swift]}: #{field[:type]}" }
  generated_lines << ""
  generated_lines << "    init("
  fields.each_with_index do |field, index|
    comma = index == fields.length - 1 ? "" : ","
    default = field[:default] ? " = #{field[:default]}" : ""
    generated_lines << "        #{field[:swift]}: #{field[:type]}#{default}#{comma}"
  end
  generated_lines << "    ) {"
  fields.each { |field| generated_lines << "        self.#{field[:swift]} = #{field[:swift]}" }
  generated_lines << "    }"
  generated_lines << ""
  generated_lines << "    enum CodingKeys: String, CodingKey {"
  fields.each do |field|
    if field[:json] == field[:swift]
      generated_lines << "        case #{field[:swift]}"
    else
      generated_lines << "        case #{field[:swift]} = #{field[:json].dump}"
    end
  end
  generated_lines << "    }"
  generated_lines << "}"
  generated_lines << ""
  generated_lines
end

lines = [
  "// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.",
  "import Foundation",
  "",
]

models.each do |schema_name, swift_struct|
  schema = resolve_schema(schemas.fetch(schema_name), schemas)
  lines.concat(render_request_model(schema_name, swift_struct, schema, schemas))
end

inline_request_models = {}
spec.fetch("paths").each_value do |path_item|
  path_item.each_value do |operation|
    next unless operation.is_a?(Hash) && operation["operationId"]
    schema = operation.dig("requestBody", "content", "application/json", "schema")
    next unless schema && !schema.key?("$ref")

    swift_struct = "#{operation.fetch("operationId").sub(/\A[a-z]/) { |first| first.upcase }}Request"
    inline_request_models[swift_struct] = schema
  end
end
inline_request_models.each do |swift_struct, schema|
  lines.concat(render_request_model(swift_struct, swift_struct, schema, schemas))
end

request_model_by_schema = models

def operation_request_type(operation, schemas, request_model_by_schema)
  schema = operation.dig("requestBody", "content", "application/json", "schema")
  return "NoRequest" unless schema
  if schema.key?("$ref")
    schema_name = schema.fetch("$ref").split("/").last
    request_model_by_schema.fetch(schema_name) do
      raise "no Swift request model mapped for OpenAPI schema #{schema_name}"
    end
  else
    "#{operation.fetch("operationId").sub(/\A[a-z]/) { |first| first.upcase }}Request"
  end
end

def operation_response_type(operation, schemas)
  success = operation.fetch("responses", {}).find { |status, _| status.match?(/\A2\d\d\z/) }
  schema = success&.last&.dig("content", "application/json", "schema")
  return "NoResponse" unless schema
  return "NoResponse" if schema["format"] == "binary"
  swift_type(schema, schemas)
end

def server_path(server)
  url = server.fetch("url")
  url.sub(%r{\Ahttps?://[^/]+}, "").sub(%r{/\z}, "")
end

def operation_path(path, operation, spec)
  escaped = path.gsub(/\{([^}]+)\}/) { "\\(#{swift_name(Regexp.last_match(1))})" }
  servers = operation["servers"]
  prefix = server_path(servers.fetch(0)) if servers && !servers.empty?
  prefix ||= server_path(spec.fetch("servers").fetch(0))
  "#{prefix}#{escaped}".sub(%r{\A/}, "")
end

# Optional query parameters are opt-in to preserve existing no-argument endpoint APIs.
def swift_query_parameters(path_item, operation, spec)
  parameters = (path_item.fetch("parameters", []) + operation.fetch("parameters", [])).map do |parameter|
    if parameter.key?("$ref")
      name = parameter.fetch("$ref").split("/").last
      spec.fetch("components").fetch("parameters").fetch(name)
    else
      parameter
    end
  end
  parameters
    .select { |parameter| parameter["in"] == "query" && (parameter["required"] || operation["x-swift-include-optional-query"]) }
    .uniq { |parameter| parameter.fetch("name") }
end

lines << "struct NoRequest: Sendable {}"
lines << "struct NoResponse: Decodable, Sendable {}"
lines << ""
lines << "struct APIOperation<Request: Sendable, Response: Sendable>: Sendable {"
lines << "    let method: String"
lines << "    let path: String"
lines << "    let queryParameters: [String: String]"
lines << "}"
lines << ""
lines << "enum APIEndpoints {"
spec.fetch("paths").each do |path, path_item|
  path_item.each do |method, operation|
    next unless %w[get post patch put delete].include?(method)
    next unless operation.is_a?(Hash) && operation["operationId"]

    name = operation.fetch("operationId")
    path_parameters = path.scan(/\{([^}]+)\}/).flatten
    generated_path = operation_path(path, operation, spec)
    query_parameters = swift_query_parameters(path_item, operation, spec)
    all_parameters = path_parameters.map { |parameter| [parameter, "String", swift_name(parameter), false] }
    query_parameters.each do |parameter|
      field = parameter.fetch("name")
      type = swift_type(parameter.fetch("schema", {"type" => "string"}), schemas)
      all_parameters << [field, type, swift_name(field), !parameter["required"]]
    end
    signature = all_parameters.map { |_, type, name, optional| optional ? "#{name}: #{type}? = nil" : "#{name}: #{type}" }.join(", ")
    query_pairs = query_parameters.map do |parameter|
      field = parameter.fetch("name")
      query_swift_name = swift_name(field)
      schema_type = swift_type(parameter.fetch("schema", {"type" => "string"}), schemas)
      value = schema_type == "String" ? query_swift_name : "String(#{query_swift_name})"
      if parameter["required"]
        "#{field.dump}: #{value}"
      else
        "#{field.dump}: #{query_swift_name}.map { String($0) }"
      end
    end.join(", ")
    query_literal = query_pairs.empty? ? "[:]" : "[#{query_pairs}]"
    if query_parameters.any? { |parameter| !parameter["required"] }
      query_literal += ".compactMapValues { $0 }"
    end
    request_type = operation_request_type(operation, schemas, request_model_by_schema)
    response_type = operation_response_type(operation, schemas)
    endpoint = "APIOperation<#{request_type}, #{response_type}>(method: #{method.upcase.dump}, path: #{generated_path.dump}, queryParameters: #{query_literal})"
    if all_parameters.empty?
      lines << "    static let #{name}: APIOperation<#{request_type}, #{response_type}> = #{endpoint}"
    else
      lines << "    static func #{name}(#{signature}) -> APIOperation<#{request_type}, #{response_type}> {"
      lines << "        APIOperation(method: #{method.upcase.dump}, path: \"#{generated_path}\", queryParameters: #{query_literal})"
      lines << "    }"
    end
  end
end
lines << "}"
lines << ""

response_lines = [
  "// Generated from openapi/openapi.yaml by scripts/generate-swift-api-models.rb. Do not edit.",
  "import Foundation",
  "",
]

response_models.each do |schema_name|
  schema = resolve_schema(schemas.fetch(schema_name), schemas)
  properties = schema.fetch("properties", {})
  required = schema.fetch("required", [])
  fields = properties.map do |field_name, field_schema|
    name = swift_name(field_name, true)
    type = swift_type(field_schema, schemas)
    is_required = required.include?(field_name)
    nullable = Array(field_schema["type"]).include?("null")
    default_present = field_schema.key?("default")
    optional = nullable || (!is_required && !default_present)
    { json: field_name, swift: name, type: optional ? "#{type}?" : type }
  end
  conformances = ["Codable", "Sendable"]
  conformances << "Identifiable" if identifiable_models.include?(schema_name)
  response_lines << "struct #{schema_name}: #{conformances.join(", ")} {"
  fields.each { |field| response_lines << "    let #{field[:swift]}: #{field[:type]}" }
  response_lines << ""
  response_lines << "    enum CodingKeys: String, CodingKey {"
  fields.each do |field|
    if field[:json] == field[:swift]
      response_lines << "        case #{field[:swift]}"
    else
      response_lines << "        case #{field[:swift]} = #{field[:json].dump}"
    end
  end
  response_lines << "    }"
  response_lines << "}"
  response_lines << ""
end

generated = lines.join("\n")
generated_responses = response_lines.join("\n")
if check_only
  unless File.file?(output_path) && File.read(output_path) == generated
    warn "#{output_path} is stale; run ruby scripts/generate-swift-api-models.rb"
    exit 1
  end
  unless File.file?(response_output_path) && File.read(response_output_path) == generated_responses
    warn "#{response_output_path} is stale; run ruby scripts/generate-swift-api-models.rb"
    exit 1
  end
else
  File.write(output_path, generated)
  File.write(response_output_path, generated_responses)
end
