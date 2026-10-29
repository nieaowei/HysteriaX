#!/usr/bin/env ruby
# Validate the public cutover boundary independently of Swift code generation.
require 'yaml'
require 'set'
root = File.expand_path('..', __dir__)
spec = YAML.load_file(File.join(root, 'openapi/openapi.yaml'), aliases: true)
def check(condition, message)
  abort message unless condition
end
check(spec.dig('info', 'version') == '2.0.0', 'expected management API version 2.0.0')
check(spec.fetch('servers').map { |s| s.fetch('url') } == ['https://{domain}/api/v2'], 'management server must use v2')
parameters = spec.dig('components', 'parameters')
key = parameters.fetch('IdempotencyKey')
check(key['in'] == 'header' && key['name'] == 'Idempotency-Key' && key['required'], 'writes require a header identity')
check(key.dig('schema', 'minLength') == 1 && key.dig('schema', 'maxLength') == 200, 'header identity must be bounded')
writes = []
spec.fetch('paths').each do |path, item|
  item.each do |method, operation|
    next unless %w[get post put patch delete head options].include?(method)
    if operation.key?('servers')
      check(operation.fetch('servers').all? { |s| s['url'] == 'https://{domain}' }, "root URL changed for #{path}")
      check(%w[/healthz /readyz /openapi.yaml /sub/{token} /hy2/auth/{node_id}/{node_token}].include?(path), "unexpected root operation #{path}")
      next
    end
    next if method == 'get'
    writes << operation.fetch('operationId')
    refs = (item.fetch('parameters', []) + operation.fetch('parameters', [])).map { |p| p['$ref'] }
    check(refs.count('#/components/parameters/IdempotencyKey') == 1, "missing or repeated header identity: #{method} #{path}")
  end
end
check(writes.length == 58 && writes.to_set.length == 58, 'expected all 58 distinct management write operations')
check(writes.include?('reverifyExecutionOperation'), 'missing explicit remote verification write')
schemas = spec.dig('components', 'schemas')
%w[/authorization-groups/preview /authorization-groups/{group_id}/preview /users/{id}/authorization-groups/preview].each do |path|
  check(!spec.fetch('paths').key?(path), "retired authorization preview route: #{path}")
end
%w[AuthorizationGroupCreate AuthorizationGroupUpdate AuthorizationGroupDelete UserAuthorizationGroupsUpdate].each do |name|
  check(!schemas.fetch(name).fetch('properties').key?('preview_token'), "obsolete preview token in #{name}")
end
check(spec.dig('paths', '/authorization-groups/{group_id}', 'put').nil?, 'full group replacement must be retired')
check(spec.dig('paths', '/authorization-groups/{group_id}', 'patch', 'operationId') == 'updateAuthorizationGroup', 'group updates must use PATCH')
check(schemas.dig('AuthorizationGroupCreate','properties').keys == ['name'], 'create accepts only a name')
check(schemas.dig('AuthorizationGroupUpdate','required') == ['expected_revision'], 'patch requires only the group version')
check(schemas.dig('AuthorizationGroupUpdate','properties').keys.sort == %w[expected_revision name add_user_ids remove_user_ids add_node_ids remove_node_ids].sort, 'patch must expose only deltas')
check(schemas.dig('ExecutionRecheckResult','required').include?('resource_blocked'), 'verification must expose its resource barrier')
schemas.each do |name, schema|
  check(!schema.fetch('properties', {}).key?('idempotency_key'), "obsolete body identity in #{name}")
end
version = schemas.fetch('APIVersion')
check(version.fetch('required').include?('directory_version') && version.fetch('required').include?('management_protocol'), 'version handshake fields must be required')
check(version.dig('properties', 'management_protocol', 'const') == 2 && version.dig('properties', 'directory_version', 'type') == 'string', 'version handshake has incorrect types')
check(schemas.dig('EventStreamHead', 'properties', 'cursor', '$ref') == '#/components/schemas/EventCursor', 'stream head must use the canonical cursor')
%w[/events /jobs/{id}/events].each do |path|
  operation = spec.dig('paths', path, 'get')
  %w[after Last-Event-ID].each do |name|
    parameter = operation.fetch('parameters').find { |p| p['name'] == name }
    check(parameter && parameter.dig('schema', '$ref') == '#/components/schemas/EventCursor', "untyped cursor #{path} #{name}")
  end
  check(operation.fetch('responses').key?('410'), "missing resynchronization response for #{path}")
end
check(spec.dig('paths', '/events/head', 'get', 'operationId') == 'getEventStreamHead', 'missing stream head operation')
puts 'Management v2 contract: 58 header identities, typed handshake/cursors, unchanged callback/download roots passed.'
