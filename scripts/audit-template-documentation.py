#!/usr/bin/env python3
"""Audit every versioned configuration page and field against the shipped schema.
Usage: python3 scripts/audit-template-documentation.py SOURCE/docs/configuration [--write]
Writes a reproducible inventory, not a claim of runtime or manual UI certification.
"""
import argparse
import collections
import hashlib
import importlib.util
import json
import pathlib
import re

ROOT = pathlib.Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('doc_order', ROOT / 'scripts/generate-template-documentation-order.py')
metadata = importlib.util.module_from_spec(spec)
spec.loader.exec_module(metadata)
COMMIT = 'af6e64c3b69e6132ebaee0e1a3d24e93903f6709'
ROOT_DEFS = {'dns': 'DNS', 'route': 'RouteOptions', 'ntp': 'NTPOptions', 'log': 'LogOptions', 'experimental': 'ExperimentalOptions'}
PAGE_DEFS = {
    'dns/rule': ['DNSRule', 'NestedDNSRule'], 'dns/rule_action': ['DNSRuleAction'],
    'route/rule': ['Rule', 'NestedRule'], 'route/rule_action': ['RuleAction'],
    'rule-set': ['RuleSet'], 'rule-set/headless-rule': ['HeadlessRule'],
    'shared/tls': ['InboundTLSOptions', 'OutboundTLSOptions'],
    'shared/dial': ['DialerOptions'], 'shared/http-client': ['HTTPClient', 'HTTPClientReference'],
    'shared/multiplex': ['InboundMultiplexOptions', 'OutboundMultiplexOptions'],
    'shared/tcp-brutal': ['BrutalOptions'], 'shared/v2ray-transport': ['V2RayTransport'],
    'shared/dns01_challenge': ['ACMEProviderDNS01Challenge'],
    'shared/certificate-provider': ['CertificateProvider'], 'network-namespace': ['NetworkNamespace'],
    'experimental/cache-file': ['CacheFileOptions'], 'experimental/clash-api': ['ClashAPIOptions'],
    'experimental/v2ray-api': ['V2RayAPIOptions'],
}
PROTOCOL_DEFS = {'inbound': 'Inbound', 'outbound': 'Outbound', 'endpoint': 'Endpoint', 'service': 'Service',
                 'dns/server': 'DNSServer', 'shared/certificate-provider': 'CertificateProvider', 'network-namespace': 'NetworkNamespace'}
HISTORIC = {'dns/fakeip', 'dns/server/legacy', 'route/geoip', 'route/geosite', 'outbound/dns', 'outbound/wireguard'}
REFERENCE_ONLY = {'index', 'schema', 'route/sniff', 'rule-set/source-format', 'rule-set/adguard',
                  'shared/pre-match', 'shared/wifi-state', 'shared/neighbor', 'inbound', 'outbound', 'endpoint', 'service', 'dns/server'}
OMITTED = {
    'dns': {'independent_cache'},
    'dns/rule': {'geosite', 'source_geoip', 'rule_set_ipcidr_match_source', 'outbound', 'geoip', 'rule_set_ip_cidr_accept_empty'},
    'dns/rule_action': {'strategy'},
    'experimental/cache-file': {'store_rdrc'},
    'experimental/clash-api': {'store_mode', 'store_selected', 'store_fakeip', 'cache_file', 'cache_id'},
    'inbound/hysteria': {'recv_window_conn', 'recv_window_client', 'max_conn_client', 'disable_mtu_discovery'},
    'inbound/tun': {'inet4_address', 'inet6_address', 'gso', 'inet4_route_address', 'inet6_route_address', 'inet4_route_exclude_address', 'inet6_route_exclude_address', 'endpoint_independent_nat'},
    'outbound/direct': {'override_address', 'override_port'},
    'route/rule': {'geosite', 'source_geoip', 'geoip', 'rule_set_ipcidr_match_source'},
    'rule-set': {'download_detour'}, 'shared/dial': {'domain_strategy'},
    'shared/listen': {'sniff', 'sniff_override_destination', 'sniff_timeout', 'domain_strategy', 'udp_disable_domain_unmapping'},
    'shared/tls': {'pq_signature_schemes_enabled', 'dynamic_record_sizing_disabled'},
}
NESTED_HEADINGS = {
    'dns': {'enabled': 'optimistic.enabled'},
    'service/api': {key: 'dashboard.' + key for key in ['enabled', 'path', 'download_url', 'http_client', 'update_interval']},
    'shared/tls': {**{key: 'ech.' + key for key in ['config', 'config_path', 'query_server_name']},
                   **{key: 'reality.' + key for key in ['handshake', 'private_key', 'public_key', 'short_id', 'max_time_difference']}},
}


def alternatives(node, schema):
    if '$ref' in node:
        yield from alternatives(schema['$defs'][node['$ref'].rsplit('/', 1)[-1]], schema)
    elif 'oneOf' in node or 'anyOf' in node:
        for child in node.get('oneOf', node.get('anyOf', [])):
            yield from alternatives(child, schema)
    else:
        yield node


def located_alternatives(node, schema, pointer):
    if '$ref' in node:
        yield from located_alternatives(schema['$defs'][node['$ref'].rsplit('/', 1)[-1]], schema, node['$ref'][1:])
    elif 'oneOf' in node or 'anyOf' in node:
        union = 'oneOf' if 'oneOf' in node else 'anyOf'
        for index, child in enumerate(node[union]):
            yield from located_alternatives(child, schema, f'{pointer}/{union}/{index}')
    else:
        yield node, pointer


def fields(node, schema, pointer='', prefix='', seen=()):
    result = collections.defaultdict(set)
    if '$ref' in node:
        ref = node['$ref']
        if ref in seen:
            return result
        return fields(schema['$defs'][ref.rsplit('/', 1)[-1]], schema, ref[1:], prefix, seen + (ref,))
    for union in ('oneOf', 'anyOf', 'allOf'):
        for i, child in enumerate(node.get(union, [])):
            for key, paths in fields(child, schema, f'{pointer}/{union}/{i}', prefix, seen).items(): result[key].update(paths)
    for key, child in node.get('properties', {}).items():
        path = f'{prefix}.{key}' if prefix else key
        prop = f'{pointer}/properties/{key}'
        result[path].add(prop)
        for nested, paths in fields(child, schema, prop, path, seen).items(): result[nested].update(paths)
    if isinstance(node.get('items'), dict):
        for key, paths in fields(node['items'], schema, pointer + '/items', prefix, seen).items(): result[key].update(paths)
    return result


def scope_for(page, schema):
    if page in ROOT_DEFS:
        name = ROOT_DEFS[page]
        return [(schema['$defs'][name], '/$defs/' + name)]
    if page in PAGE_DEFS:
        return [(schema['$defs'][name], '/$defs/' + name) for name in PAGE_DEFS[page]]
    if page == 'certificate': return [(schema['properties']['certificate'], '/properties/certificate')]
    if page == 'shared/listen':
        return [(schema['$defs'][name], '/$defs/' + name) for name in ['Inbound', 'Service', 'Endpoint']]
    if page in ['shared/http2', 'shared/quic']: return [(schema['$defs']['HTTPClient'], '/$defs/HTTPClient')]
    if page == 'shared/udp-nat': return [(schema['$defs']['Inbound'], '/$defs/Inbound'), (schema['$defs']['Endpoint'], '/$defs/Endpoint')]
    if page == 'shared/udp-over-tcp': return [(schema['$defs']['Outbound'], '/$defs/Outbound')]
    for directory, name in PROTOCOL_DEFS.items():
        if page.startswith(directory + '/'):
            kind = page[len(directory) + 1:]
            kind = {'http3': 'h3'}.get(kind, kind)
            selected = []
            for node, pointer in located_alternatives(schema['$defs'][name], schema, '/$defs/' + name):
                tag = node.get('properties', {}).get('type', {})
                if tag.get('const') == kind or kind in tag.get('enum', []): selected.append((node, pointer))
            return selected
    return []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=pathlib.Path)
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args()
    try:
        metadata.verify_source(args.source)
    except ValueError as error:
        parser.error(str(error))
    schema_path = ROOT / 'crates/hysteriax-server/src/api/subscriptions/templates/singbox.schema.json'
    schema_bytes = schema_path.read_bytes()
    if hashlib.sha256(schema_bytes).hexdigest() != metadata.SCHEMA_SHA256:
        parser.error('Shipped schema no longer matches the 1.14.2 audit baseline')
    if (args.source.parent / 'schema.json').read_bytes() != schema_bytes:
        parser.error('Upstream source schema differs from the shipped schema')
    schema = json.loads(schema_bytes)
    pages = []
    unmatched = collections.defaultdict(list)
    for doc in sorted(args.source.rglob('*.md')):
        if doc.name.endswith('.zh.md'): continue
        page = doc.relative_to(args.source).with_suffix('').as_posix()
        if page.endswith('/index'): page = page[:-6]
        content = doc.read_text()
        keys, anchors = metadata.extract(content)
        scope = scope_for(page, schema)
        candidates = collections.defaultdict(set)
        for node, pointer in scope:
            for key, paths in fields(node, schema, pointer).items(): candidates[key].update(paths)
        # UoT is a shared inline object rather than a named definition.
        if page == 'shared/udp-over-tcp':
            candidates = {key.removeprefix('udp_over_tcp.'): paths for key, paths in candidates.items() if key.startswith('udp_over_tcp.')}
        rows = []
        for key in keys:
            lookup = NESTED_HEADINGS.get(page, {}).get(key, key)
            matches = sorted(candidates.get(lookup, []))
            status = 'schema' if matches else 'unresolved'
            reason = '固定 Schema 有对应字段；具体控件及条件显示由 SwiftUI 和模型回归验证。' if matches else ''
            if page in HISTORIC:
                status, reason = 'historic', '上游保留的历史迁移文档；不在当前 1.14.2 类型目录中新增旧配置。'
            elif page in REFERENCE_ONLY:
                status, reason = 'reference', '说明、动作名或独立规则集文件格式，不是订阅根配置的新字段。'
            elif page.startswith('outbound/') and page.split('/')[-1] not in ['direct', 'selector', 'urltest']:
                status, reason = 'policy', '产品策略限制：手工出站只开放 direct/selector/urltest；Hysteria2 连接信息由节点注入。'
            elif not matches:
                # A field heading may be an action/transport discriminator value rather than a property.
                kinds = {str(x.get('properties', {}).get(k, {}).get('const')) for node, _ in scope for x in alternatives(node, schema) for k in ['type', 'action']}
                if key in kinds or (page == 'shared/v2ray-transport' and key == 'gRPC'):
                    status, reason = 'discriminator', '这是类型/动作取值标题；对应 Schema 联合类型，不是独立属性。'
                elif page == 'shared/tls' and key in {'domain', 'data_directory', 'default_server_name', 'email', 'provider', 'disable_http_challenge', 'disable_tls_alpn_challenge', 'alternative_http_port', 'alternative_tls_port', 'external_account', 'external_account.key_id', 'external_account.mac_key', 'dns01_challenge'}:
                    status, reason = 'migration', '旧内联 ACME 段落；当前 Schema 使用共享 certificate_providers，不能重新加入旧 TLS 表单。'
                elif key in OMITTED.get(page, set()):
                    status, reason = 'omitted', '文档保留的废弃、移除或 schema:omit 字段；固定 Schema 不提供受验证表单，已有值仍保留并提示未验证。'
                elif page == 'dns/server/fakeip' and key == 'inet6_address':
                    status, reason = 'upstream_gap', '文档标题为 inet6_address；同提交 Schema 和原生选项均为 inet6_range。编辑器遵循 Schema。'
                elif page == 'endpoint/openvpn-server' and key in ['users.username', 'users.password']:
                    status, reason = 'upstream_gap', '文档使用小写用户名/密码；上游 User 定义使用 Username/Password。保留固定 Schema，大小写差异单列，不能视为逐字一致。'
                else:
                    unmatched[page].append(key)
            rows.append({'field': key, 'status': status, 'reason': reason, 'schema_paths': matches,
                         'anchor': anchors.get(key, key.replace('.', '').lower())})
        pages.append({'page': page, 'source': f'https://github.com/SagerNet/sing-box/blob/{COMMIT}/docs/configuration/{doc.relative_to(args.source).as_posix()}',
                      'source_sha256': hashlib.sha256(doc.read_bytes()).hexdigest(), 'fields': rows})
    counts = collections.Counter(row['status'] for page in pages for row in page['fields'])
    for page in pages:
        for row in page['fields']:
            for pointer in row['schema_paths']:
                node = schema
                for key in pointer.lstrip('/').split('/'):
                    node = node[int(key)] if isinstance(node, list) else node[key]
    output = {'version': 1, 'kernel': '1.14.2', 'commit': COMMIT, 'schema_sha256': metadata.SCHEMA_SHA256,
              'documentation_sha256': metadata.DOCUMENTATION_SHA256,
              'page_count': len(pages), 'field_count': sum(len(p['fields']) for p in pages), 'counts': dict(sorted(counts.items())), 'pages': pages}
    print(json.dumps({k: v for k, v in output.items() if k != 'pages'}, ensure_ascii=False, indent=2))
    if unmatched: print('NEEDS REVIEW: ' + json.dumps(unmatched, ensure_ascii=False, sort_keys=True))
    if args.write:
        dest = ROOT / 'docs/audits'
        dest.mkdir(exist_ok=True)
        (dest / '0023-singbox-documentation-inventory.json').write_text(json.dumps(output, ensure_ascii=False, indent=2) + '\n')
        lines = ['# 0023 sing-box 配置逐字段核查附录', '', '由 `scripts/audit-template-documentation.py` 从固定提交生成。', '',
                 f'基线：1.14.2 / `{COMMIT}`；{len(pages)} 页，{output["field_count"]} 个页内不重复字段/动作标题。', '',
                 '此附录核查文档与 Schema 的对应关系；`schema` 仅表示类型定义存在，不能等同于每项运行时语义或视觉验收均已验证。具体优化、测试和限制见主报告。', '',
                 '状态：schema=Schema 对应；policy=产品策略限制；historic=历史迁移页；migration=旧语法迁移；reference=说明或独立格式；discriminator=类型/动作标题；omitted=上游省略/废弃字段；upstream_gap=上游文档与 Schema 差异；unresolved=需审查差异。', '']
        for page in pages:
            lines += ['## ' + page['page'], '', '[固定版本原文](' + page['source'] + ')', '']
            if not page['fields']: lines += ['本页无可提取的配置字段标题；已纳入页面清单。', '']; continue
            lines += ['| 字段/标题 | 核查结果 | 说明 |', '| --- | --- | --- |']
            for row in page['fields']:
                lines += ['| `' + row['field'] + '` | ' + row['status'] + ' | ' + row['reason'] + ' |']
            lines.append('')
        (dest / '0023-singbox-documentation-fields.md').write_text('\n'.join(lines))
    if unmatched: raise SystemExit(1)


if __name__ == '__main__': main()
