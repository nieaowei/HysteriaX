#!/usr/bin/env python3
"""Snapshot all official Chinese configuration navigation pages and compare fields.
Usage: python3 scripts/audit-template-website.py --as-of YYYY-MM-DD [--write]
Only identifiers, URLs and hashes are retained, not a copy of the documentation.
"""
import argparse
import concurrent.futures
import hashlib
from html.parser import HTMLParser
import json
import pathlib
import re
import urllib.parse
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parents[1]
BASE = 'https://sing-box.sagernet.org/zh/configuration/'
IDENTIFIER = re.compile(r'[a-z][a-zA-Z0-9_]*(?:\.[a-zA-Z0-9_]+)*(?:, *[a-z][a-zA-Z0-9_.]*)*')


class Page(HTMLParser):
    def __init__(self):
        super().__init__()
        self.links, self.headings, self.text = [], [], []
        self.article = False
        self.heading = None

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag == 'article': self.article = True
        if tag == 'a' and 'href' in attrs: self.links.append(attrs['href'])
        if self.article and re.fullmatch(r'h[2-6]', tag): self.heading = []

    def handle_endtag(self, tag):
        if self.heading is not None and re.fullmatch(r'h[2-6]', tag):
            self.headings.append(''.join(self.heading).replace('¶', '').strip())
            self.heading = None
        if tag == 'article': self.article = False

    def handle_data(self, data):
        if self.article: self.text.append(data)
        if self.heading is not None: self.heading.append(data)


def fetch(url):
    request = urllib.request.Request(url, headers={'User-Agent': 'HysteriaX-Documentation-Audit/1.0'})
    with urllib.request.urlopen(request, timeout=40) as response:
        raw = response.read()
    page = Page()
    page.feed(raw.decode('utf-8'))
    if not page.text: raise ValueError('No article content in ' + url)
    return raw, page


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--as-of', required=True)
    parser.add_argument('--write', action='store_true')
    args = parser.parse_args()
    raw, root = fetch(BASE)
    urls = {BASE}
    for link in root.links:
        url = urllib.parse.urljoin(BASE, link).split('#')[0].split('?')[0]
        if url.startswith(BASE) and not pathlib.PurePosixPath(urllib.parse.urlparse(url).path).suffix:
            urls.add(url.rstrip('/') + '/')
    pinned = json.loads((ROOT / 'docs/audits/0023-singbox-documentation-inventory.json').read_text())
    baseline = {p['page']: {r['field'] for r in p['fields']} for p in pinned['pages']}
    pages = []
    def inspect(url):
        payload, page = (raw, root) if url == BASE else fetch(url)
        name = url[len(BASE):].strip('/') or 'index'
        fields = list(dict.fromkeys(key.strip() for h in page.headings if IDENTIFIER.fullmatch(h) for key in h.split(',')))
        current, old = set(fields), baseline.get(name, set())
        text = ''.join(page.text)
        return {'page': name, 'url': url, 'source_sha256': hashlib.sha256(payload).hexdigest(),
                'fields': fields, 'added_fields': sorted(current - old), 'absent_fields': sorted(old - current),
                'version_1_15_mentioned': bool(re.search(r'1\.15\.0', text)), 'new_page': name not in baseline}
    with concurrent.futures.ThreadPoolExecutor(max_workers=8) as executor:
        pages = list(executor.map(inspect, sorted(urls)))
    output = {'as_of': args.as_of, 'baseline': pinned['kernel'], 'page_count': len(pages),
              'new_pages': [p['page'] for p in pages if p['new_page']],
              'pinned_pages_not_in_navigation': sorted(set(baseline) - {p['page'] for p in pages}), 'pages': pages}
    print(json.dumps({k: v for k, v in output.items() if k != 'pages'}, ensure_ascii=False, indent=2))
    for page in pages:
        if page['added_fields'] or page['absent_fields']:
            print(page['page'] + ' + ' + ' '.join(page['added_fields']) + ' - ' + ' '.join(page['absent_fields']))
    if args.write:
        (ROOT / 'docs/audits/0023-singbox-website-delta.json').write_text(json.dumps(output, ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__': main()
