import copy,json,pathlib,random,sys
import yaml
ROOT=pathlib.Path(sys.argv[1])
OUT=pathlib.Path(sys.argv[2]);OUT.mkdir(parents=True,exist_ok=True)
random.seed(5006)

def put(doc,path,value):
    cur=doc
    for key in path[:-1]:cur=cur[key]
    cur[path[-1]]=value

def paths(v,p=()):
    yield p,v
    if isinstance(v,dict):
        for k,x in v.items():yield from paths(x,p+(k,))
    elif isinstance(v,list):
        for k,x in enumerate(v):yield from paths(x,p+(k,))

for fmt in ['mihomo','singbox']:
    cases=[]
    def add(name,value,expected=None,bench=False):
        item={'name':name,'config':value,'bench':bench}
        if expected is not None:item['expected']=expected
        cases.append(item)
    if fmt=='mihomo':
        seed=yaml.safe_load((ROOT/'mihomo-template.yaml').read_text())
        node=yaml.safe_load((ROOT/'mihomo-hysteria2.yaml').read_text())['proxies'][0]
        node['name']='hx:fixture:node'
        node['hop-interval']=30;node['ports']='443,5000-5100';node['ech-opts']={'enable':True,'config':'fixture-ech'}
        node['certificate']='fixture-cert';node['private-key']='fixture-key'
        seed['proxies']=[node];seed['proxy-groups'][0]['proxies']=[node['name'],'DIRECT']
        for platform in ['linux','macos','windows','android','ios']:
            value=copy.deepcopy(seed)
            if platform not in ['linux','macos']:value.pop('redir-port',None)
            add('template-'+platform,value,True,platform=='linux')
        base=copy.deepcopy(cases[0]['config'])
        future=copy.deepcopy(base);future['future-field']={'counter':18446744073709551615};future['proxies'][0]['future-option']=True
        add('unknown-extensions',future,True)
        for v in [0,65535,7890.0]:
            d=copy.deepcopy(base);d['mixed-port']=v;add('port-valid-'+str(v),d,True)
        for v in [-1,65536,1.5,'7890',None,True]:
            d=copy.deepcopy(base);d['mixed-port']=v;add('port-invalid-'+str(v),d,False)
        for missing in ['name','type','server','password']:
            d=copy.deepcopy(base);del d['proxies'][0][missing];add('required-'+missing,d,False)
        for level in ['INFO','silent','invalid']:
            d=copy.deepcopy(base);d['log-level']=level;add('log-level-'+level,d,level!='invalid')
        for count in [10,100,500]:
            d=copy.deepcopy(base);d['proxies']=[dict(copy.deepcopy(node),name=f'hx:fixture:node{i}') for i in range(count)]
            d['proxy-groups'][0]['proxies']=[n['name'] for n in d['proxies']];add('nodes-'+str(count),d,True,count==100)
    else:
        seed=json.loads((ROOT/'singbox-template.json').read_text())
        node={'type':'hysteria2','tag':'hx:fixture:node','server':'example.invalid','server_port':443,'password':'fixture-password','up_mbps':100,'down_mbps':200,'disable_chrome_parrot':True,'tls':{'enabled':True,'insecure':True,'server_name':'example.invalid','client_certificate':['fixture-cert'],'client_key':['fixture-key'],'ech':{'enabled':True,'config':['fixture-ech']}},'obfs':{'type':'gecko','password':'fixture-obfs','min_packet_size':512,'max_packet_size':1200}}
        for g in seed['outbounds']:
            if isinstance(g.get('outbounds'),list):g['outbounds']=[node['tag'] if x.startswith('{{') else x for x in g['outbounds']]
            if isinstance(g.get('default'),str) and g['default'].startswith('{{'):g['default']=node['tag']
        seed['outbounds'].append(node)
        for platform in ['linux','macos','windows','android','ios']:
            d=copy.deepcopy(seed)
            d['inbounds']=[i for i in d.get('inbounds',[]) if (i['type']!='tproxy' or platform=='linux') and (i['type']!='redirect' or platform in ['linux','macos'])]
            for inbound in d['inbounds']:
                if inbound['type']=='tun':
                    if platform!='linux':inbound.pop('auto_redirect',None)
                    if platform in ['macos','android','ios']:inbound.pop('interface_name',None)
                    if platform in ['android','ios']:inbound.pop('auto_route',None)
            add('template-'+platform,d,True,platform=='linux')
        base=copy.deepcopy(cases[0]['config'])
        d=copy.deepcopy(base);d['future-field']={'counter':18446744073709551615};d['outbounds'][-1]['tls']['future-field']=True;add('unknown-extensions',d,True)
        d=copy.deepcopy(base);d['log']={'level':123};add('log-invalid-type',d,False)
        for v in ['info','invalid',None]:
            d=copy.deepcopy(base);d['log']={'level':v};add('log-level-'+str(v),d,v=='info')
        d=copy.deepcopy(base);d['outbounds'].append({'type':'direct','tag':'bad-direct','outbounds':None});add('wrong-union-member',d,False)
        d=copy.deepcopy(base);d['outbounds'][-1].pop('type');add('missing-outbound-type',d,False)
        for depth in [1,4,16,32]:
            rule={'domain':['example.invalid'],'action':'route','outbound':'direct'}
            for _ in range(depth):rule={'type':'logical','mode':'and','rules':[rule],'action':'route','outbound':'direct'}
            d=copy.deepcopy(base);d['route']['rules']=[rule];add('nested-route-'+str(depth),d,None,depth==16)
        for count in [10,100,500]:
            d=copy.deepcopy(base);d['outbounds']=[g for g in d['outbounds'] if g.get('type')!='hysteria2']+[dict(copy.deepcopy(node),tag=f'hx:fixture:node{i}') for i in range(count)]
            add('nodes-'+str(count),d,True,count==100)
    items=list(paths(base));random.shuffle(items)
    for path,old in items[:125]:
        if not path:continue
        replacements=[None,{'bad':True},['bad']]
        if not isinstance(old,str):replacements.append('invalid')
        else:replacements.append(123)
        for n,new in enumerate(replacements):
            d=copy.deepcopy(base);put(d,path,new);add('mutate-'+('/'.join(map(str,path)))+'-'+str(n),d)
    (OUT/f'{fmt}.json').write_text(json.dumps(cases,ensure_ascii=False,indent=2)+'\n')
    print(fmt,len(cases))

fixtures=OUT.parent/'fixtures';fixtures.mkdir(exist_ok=True)
for fmt in ['mihomo','singbox']:
    cases=json.loads((OUT/f'{fmt}.json').read_text())
    (fixtures/f'{fmt}.json').write_text(json.dumps(cases[0]['config'],ensure_ascii=False))
