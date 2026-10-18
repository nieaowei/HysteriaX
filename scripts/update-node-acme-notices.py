#!/usr/bin/env python3
"""Refresh redistributable dependency license texts from the locked Go modules."""
import json
from pathlib import Path
import re
import subprocess

root = Path(__file__).resolve().parent.parent
component = root / 'components/hysteriax-acme'
subprocess.run(['go','mod','download'],cwd=component,check=True)
result = subprocess.check_output(['go','list','-deps','-json','.'],cwd=component,text=True)
decoder = json.JSONDecoder()
modules = []
while result.strip():
    result = result.lstrip()
    value, count = decoder.raw_decode(result)
    if value.get("Module") and value["Module"] not in modules: modules.append(value["Module"])
    result = result[count:]
licenses = component / 'third_party'
licenses.mkdir(exist_ok=True)
lines = ['# Node ACME dependency notices', '', 'HysteriaX is MIT licensed; see `LICENSE`. The component uses CertMagic v0.25.4 and the eight libdns provider versions used by unmodified Hysteria v2.12.3. The module graph and checksums are locked in `go.mod` / `go.sum`. Provider mapping follows the upstream MIT-licensed Hysteria application.', '', 'Dependency license and notice texts are reproduced below by module. Regenerate with `python3 scripts/update-node-acme-notices.py` after a dependency change. Pebble is an optional test tool and is not linked into the distributed component.', '', '| Module | Version | License / notices |', '| --- | --- | --- |']
for module in modules:
    if module.get('Main'): continue
    path = module.get('Dir')
    if not path: raise RuntimeError('missing source for '+module['Path'])
    source = Path(path)
    files = [f for f in source.iterdir() if f.is_file() and re.match(r'^(LICENSE|LICENCE|COPYING|NOTICE)(\.|$|[-_])',f.name,re.I)]
    if not files: raise RuntimeError('missing license for '+module['Path'])
    references=[]
    for file in sorted(files):
        destination=licenses / (module['Path'].replace('/','_')+'@'+module['Version']+'_'+file.name)
        destination.write_bytes(file.read_bytes())
        references.append('['+file.name+'](third_party/'+destination.name+')')
    lines.append('| `'+module['Path']+'` | `'+module['Version']+'` | '+', '.join(references)+' |')
(component/'THIRD_PARTY_NOTICES.md').write_text('\n'.join(lines)+'\n')
print('Updated notices for',len(modules)-1,'locked modules')
