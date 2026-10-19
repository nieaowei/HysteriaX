import copy,json,pathlib,sys
r=pathlib.Path(sys.argv[1]);out=r/'edges';out.mkdir(exist_ok=True)
for fmt in ['mihomo','singbox']:
 base=json.loads((r/'fixtures'/f'{fmt}.json').read_text());cases=[]
 def add(name,d,expected):cases.append({'name':name,'config':d,'expected':expected,'bench':False})
 if fmt=='mihomo':
  for interval,expected in [('30',True),(30,True),(30.5,False)]:
   d=copy.deepcopy(base);d['proxies'][0]['hop-interval']=interval;add('hopping-'+str(interval),d,expected)
  d=copy.deepcopy(base);d['proxies'][0]['ech-opts']['enable']='true';add('ech-invalid-type',d,False)
  d=copy.deepcopy(base);d['proxies'][0]['realm-opts']['stun-servers']=123;add('realm-invalid-stun',d,False)
 else:
  d=copy.deepcopy(base);n=d['outbounds'][-1];n.pop('server_port',None);n['server_ports']=['443:443','5000:5100'];n['hop_interval']='30s';add('hopping',d,True)
  d=copy.deepcopy(base);n=d['outbounds'][-1];n.pop('server',None);n.pop('server_port',None);n['realm']={'server_url':'https://realm.example.invalid','token':'fixture-realm','realm_id':'fixture-realm','stun_servers':['stun.example.invalid:3478'],'http_client':{'tls':{'enabled':True,'insecure':True}}};n['tls']['handshake_timeout']='30s';add('realm-mtls-ech',d,True)
  d=copy.deepcopy(base);d['outbounds'][-1]['tls']['ech']['enabled']='true';add('ech-invalid-type',d,False)
  d=copy.deepcopy(base);d['outbounds'][-1]['tls']['client_certificate']=[123];add('mtls-invalid-certificate',d,False)
  d=copy.deepcopy(base);d['outbounds'][-1]['obfs']['min_packet_size']='512';add('gecko-invalid-packet-size',d,False)
 (out/f'{fmt}.json').write_text(json.dumps(cases,ensure_ascii=False,indent=2))
