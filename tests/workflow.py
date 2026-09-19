"""Real plugin process + mock host services; no production credentials or upstream calls."""
import http.server,json,os,pathlib,struct,subprocess,sys,tempfile,threading,time
account={'id':'acct_fixture','name':'Fixture','plan':'plus','eligible':True,'binding':'a'*64,'revision':1,'authenticationKind':'oauth'}
state='gAAAAA'+'x'*286
calls=[]
class Host(http.server.BaseHTTPRequestHandler):
 def do_POST(self):
  assert self.headers['Authorization']=='Bearer fixture'
  req=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
  method=req['method'];data=req['input'];calls.append(method)
  if method=='accounts.list':result=[account]
  elif method=='accounts.get':result=account
  elif method=='responses.probe':
   assert data['credentialScope']==account['binding'];result=[200,state,0,'fixture']
  elif method=='network.exit':result='192.0.2.10'
  elif method=='proxies.resolve':result='http://fixture:secret@127.0.0.1:8080'
  elif method=='proxies.list':result={'items':[{'id':'saved','name':'Saved','endpoint':'http://127.0.0.1:8080','hasAuthentication':True}],'page':{'totalPages':1}}
  else:raise AssertionError(method)
  body=json.dumps(result).encode();self.send_response(200);self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
 def log_message(self,*args):pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Host)
threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory() as root:
 def start():
  return subprocess.Popen([sys.argv[1]],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env={**os.environ,'RS_PLUGIN_DATA_DIR':root,'RS_PLUGIN_SERVICES_URL':f'http://127.0.0.1:{server.server_port}/invoke','RS_PLUGIN_SERVICES_TOKEN':'Bearer fixture'})
 proc=start();serial=0
 def rpc(method,params={}):
  global serial
  serial+=1;body=json.dumps({'jsonrpc':'2.0','id':serial,'method':method,'params':params}).encode();proc.stdin.write(struct.pack('>I',len(body))+body);proc.stdin.flush()
  prefix=proc.stdout.read(4);assert len(prefix)==4,proc.stderr.read().decode()
  result=json.loads(proc.stdout.read(struct.unpack('>I',prefix)[0]));assert 'error' not in result,result
  return result['result']
 def job(method,params):
  id=rpc(method,params)['jobId']
  for _ in range(300):
   result=rpc('admin.job',{'id':id})
   if not result.get('pending'):assert 'error' not in result,result;return result['result']
   time.sleep(.05)
  raise AssertionError('job timed out')
 def context():return {'provider':'openai','accountId':account['id'],'model':'gpt-6-astra','credentialScope':account['binding'],'authenticationKind':'oauth','planType':'plus','accountEligible':True}
 try:
  rpc('initialize',{'apiVersion':1,'pluginId':'turn-state'})
  panel=rpc('admin.panel');settings=panel['settings'];settings.update(enabled=True,inject=True,requireTicket=True,manualIntervalSeconds=0,models=['gpt-6-astra'],accounts={'acct_fixture':{'mode':'manual','targetLength':None}})
  panel=rpc('admin.update',{'revision':panel['revision'],'settings':settings,'proxies':[{'name':'Saved','savedProxyId':'saved','enabled':True,'concurrency':2}]})
  assert rpc('request.before_send',context())['deny']
  result=job('admin.probe',{'accountId':account['id'],'model':'gpt-6-astra'});assert result['matched'],result
  assert rpc('request.before_send',context())['values']['session_state']==state
  assert job('admin.exit',{'revision':panel['revision'],'proxyId':'0'})['ip']=='192.0.2.10'
  panel=rpc('admin.panel');assert len(panel['logs'])==1;assert 'secret' not in json.dumps(panel);assert state not in json.dumps(panel)
  rpc('admin.clear_logs');assert rpc('admin.panel')['logs']==[]
  account['binding']='b'*64;assert rpc('request.before_send',context())['deny']
  rpc('admin.continuous',{'accountId':account['id'],'model':'gpt-6-astra','intervalSeconds':10})
  for _ in range(300):
   panel=rpc('admin.panel')
   if panel['accounts'][0]['models'][0]['ready'] and not panel['accounts'][0]['models'][0]['continuous']:break
   time.sleep(.05)
  else:raise AssertionError('continuous did not complete')
  account['binding']='c'*64;settings=panel['settings'];settings['accounts'][account['id']]['mode']='auto';settings['activityOnly']=True;settings['intervalSeconds']=10
  panel=rpc('admin.update',{'revision':panel['revision'],'settings':settings})
  before=calls.count('responses.probe');time.sleep(1.2);assert calls.count('responses.probe')==before
  rpc('request.before_send',context())
  for _ in range(300):
   if rpc('admin.panel')['accounts'][0]['models'][0]['ready']:break
   time.sleep(.05)
  else:raise AssertionError('demand auto did not complete')
  proc.stdin.close();proc.wait(timeout=5);assert proc.returncode==0
  proc=start();rpc('initialize',{'apiVersion':1,'pluginId':'turn-state'});assert rpc('request.before_send',context())['values']['session_state']==state
  assert pathlib.Path(root,'turn-state-tickets.json').exists()
  print('PASS manual, persistence/restart, identity gate, proxy import, exit sample, logs, continuous concurrency, demand auto')
 finally:
  proc.kill();proc.wait();server.shutdown()
