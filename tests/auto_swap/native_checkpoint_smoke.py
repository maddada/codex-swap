import importlib.machinery,importlib.util,json,os,pathlib,shutil,signal,subprocess,sys,tempfile,threading,time
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
SOURCE=pathlib.Path(__file__).resolve().parents[2] / 'scripts/codex-auto'
loader=importlib.machinery.SourceFileLoader('checkpoint_smoke',str(SOURCE));spec=importlib.util.spec_from_loader(loader.name,loader);auto=importlib.util.module_from_spec(spec);loader.exec_module(auto)
requests=[]
class Handler(BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_POST(self):
  self.rfile.read(int(self.headers.get('Content-Length','0')));requests.append(self.path)
  item={'id':'msg_smoke','type':'message','role':'assistant','status':'completed','content':[{'type':'output_text','text':'OK','annotations':[]}]}
  response={'id':'resp_smoke','object':'response','created_at':int(time.time()),'status':'completed','model':'smoke-model','output':[item],'usage':{'input_tokens':1,'output_tokens':1,'total_tokens':2}}
  events=[{'type':'response.created','response':dict(response,status='in_progress',output=[])},{'type':'response.output_item.added','output_index':0,'item':dict(item,status='in_progress',content=[])},{'type':'response.output_text.delta','item_id':'msg_smoke','output_index':0,'content_index':0,'delta':'OK'},{'type':'response.output_item.done','output_index':0,'item':item},{'type':'response.completed','response':response}]
  body=''.join('event: '+e['type']+'\ndata: '+json.dumps(e)+'\n\n' for e in events).encode()
  self.send_response(200);self.send_header('Content-Type','text/event-stream');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
server=ThreadingHTTPServer(('127.0.0.1',0),Handler);threading.Thread(target=server.serve_forever,daemon=True).start()
with tempfile.TemporaryDirectory(prefix='checkpoint space-🧪-') as temporary:
 root=pathlib.Path(temporary);home=root/'home';home.mkdir();gate_dir=root/'gate';gate_dir.mkdir();seen=root/'hook.json';bootstrap=root/'bootstrap.py'
 config='model = "smoke-model"\nmodel_provider = "smoke"\n[model_providers.smoke]\nname = "Smoke"\nwire_api = "responses"\nbase_url = "http://127.0.0.1:%d/v1"\nrequires_openai_auth = false\n' % server.server_port
 (home/'config.toml').write_text(config)
 bootstrap.write_text('import json,pathlib,sys,subprocess\npayload=sys.stdin.buffer.read()\nd=json.loads(payload)\npathlib.Path('+repr(str(seen))+').write_bytes(payload)\npathlib.Path('+repr(str(gate_dir/'handoff-gate.json'))+').write_text(json.dumps(dict(session=d["session_id"],owner='+str(os.getpid())+',nonce="native-smoke")))\nsys.exit(subprocess.run(['+repr(sys.executable)+','+repr(str(SOURCE))+',"--checkpoint",'+repr(str(gate_dir))+'],input=payload).returncode)\n')
 import shlex
 codex=os.environ.get('CODEX_AUTO_REAL_CODEX') or shutil.which('codex')
 adapter=root/'xswap-probe'
 adapter.write_text('#!'+sys.executable+'\nimport os,sys\nos.environ["CODEX_HOME"]='+repr(str(home))+'\nargs=sys.argv[sys.argv.index("--")+1:]\nos.execv('+repr(codex)+',['+repr(codex)+']+args)\n');adapter.chmod(0o700)
 existing_marker=root/'existing-hook-ran'
 existing_command=shlex.join([sys.executable,'-c','from pathlib import Path; Path('+repr(str(existing_marker))+').write_text("ran")'])
 existing=[{'hooks':[{'type':'command','command':existing_command,'timeout':10}]}]
 groups=auto.effective_pre_hooks(str(adapter),codex,{'alias':'probe','number':1},['-C',str(root),'-c','hooks.PreModelRequest='+auto.toml_inline(existing)])
 assert groups[0]['hooks'][0]['command']==existing_command,'Effective configuration lost the existing hook'
 groups += [{'hooks':[{'type':'command','command':shlex.join([sys.executable,str(bootstrap)]),'timeout':60,'async':False}]}]
 command=[os.environ.get('CODEX_AUTO_REAL_CODEX') or shutil.which('codex'),'exec','--json','--skip-git-repo-check','--enable','hooks','--dangerously-bypass-hook-trust','-c','hooks.PreModelRequest='+auto.toml_inline(groups),'Reply only OK. Do not run any tools.']
 proc=subprocess.Popen(command,cwd=root,env={**os.environ,'CODEX_HOME':str(home)},stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
 try:
  deadline=time.monotonic()+20
  while time.monotonic()<deadline and not (gate_dir/'handoff-ready.json').exists() and proc.poll() is None:time.sleep(.05)
  if not (gate_dir/'handoff-ready.json').exists():
   proc.terminate();out,err=proc.communicate(timeout=5);raise AssertionError('Native callback not held: '+err.decode()[-1800:])
  payload=json.loads(seen.read_text());gate=auto.HandoffGate(gate_dir);gate.request=json.loads(gate.path.read_text());record=gate.held(payload['session_id'])
  assert record and record['turn']==payload['turn_id'],record
  assert not requests,'Model request escaped the held native checkpoint'
  assert existing_marker.exists(),'Existing native hook did not run'
  conv=auto.Conversation(payload['session_id'],home,since=0);conv.events()
  assert conv.turn==payload['turn_id'],('Turn was not persisted before checkpoint',conv.turn,payload['turn_id'])
  gate.cancel()
  out,err=proc.communicate(timeout=30)
  assert requests,'Native model request did not resume after releasing checkpoint'
  assert proc.returncode==0,(proc.returncode,err.decode()[-1000:],out.decode()[-1000:])
  print(json.dumps({'native_hook_held':True,'existing_effective_hook_preserved':True,'request_blocked_until_release':True,'turn_persisted_before_callback':True,'model_finished_after_release':True,'external_model_requests':False}))
 finally:
  if proc.poll() is None:os.killpg(proc.pid,signal.SIGKILL);proc.wait()
server.shutdown()
