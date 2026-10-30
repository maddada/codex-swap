import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts/codex-auto'
loader = importlib.machinery.SourceFileLoader('auto_swap', str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
auto = importlib.util.module_from_spec(spec)
loader.exec_module(auto)
A = '00000000-0000-7000-8000-000000000001'
B = '10000000-0000-7000-8000-000000000002'
FAKE = r'''#!/usr/bin/env python3
import datetime, json, os, pathlib, select, sqlite3, sys, time, tty
root=pathlib.Path(os.environ['FAKE_ROOT']); args=sys.argv[1:]; mode=os.environ['FAKE_MODE']
accounts=[dict(alias=a,number=i+1,home=str(root/'home'),enabled=True,loginStatus='present',isDefault=i==0) for i,a in enumerate(['alt','personal','work'])]
if args[0]=='--codex-bin': args=args[2:]
if args[0]=='list': print(json.dumps({'accounts':accounts}));sys.exit()
if args[0]=='status': print(json.dumps({'defaultHome':str(root/'main')}));sys.exit()
if args[0]=='usage':
 values=json.loads((root/'usage.json').read_text())
 print(json.dumps({'accounts':[dict(number=a['number'],usage=dict(allowed=True,limitReached=False,windows=[dict(scope='codex',usedPercent=values[a['alias']])])) for a in accounts]}));sys.exit()
assert args[0]=='run' and '--share-history' in args and '--no-daemon' in args
assert os.environ['XSWAP_STOP_CODEX']=='never' and os.getpgrp()==os.getpid()
with (root/'launches').open('a') as f:f.write(json.dumps(args)+'\n')
logdir=pathlib.Path(json.loads(next(x for x in args if x.startswith('log_dir=')).split('=',1)[1]));logdir.mkdir(parents=True,exist_ok=True)
log=logdir/'codex-tui.log'
a='00000000-0000-7000-8000-000000000001';b='10000000-0000-7000-8000-000000000002';child='20000000-0000-7000-8000-000000000003'
sid=b if mode=='chat-switch' else a
home=root/'home/sessions/2026/10/30';home.mkdir(parents=True,exist_ok=True)
def meta(s,parent=None):
 p=home/('rollout-'+s+'.jsonl')
 if not p.exists():p.write_text(json.dumps({'type':'session_meta','payload':{'id':s,'cwd':str(root/'project'),'parent_thread_id':parent}})+'\n')
 return p
def event(p,kind,**fields):
 d={'timestamp':datetime.datetime.now(datetime.timezone.utc).isoformat(),'type':'event_msg','payload':dict(type=kind,**fields)}
 with p.open('a') as f:f.write(json.dumps(d)+'\n')
def title(s):
 os.write(1,('\x1b]0;'+s[:29]+'... | fixture\x07\x1b]7501;state=idle:app=codex\x1b\\').encode())
def announce(s):
 with log.open('a') as f:f.write('INFO session_loop{thread_id='+s+'}: codex_core::session: new\n')
 title(s)
if args[1]=='alt':
 p=meta(a);announce(a)
 if mode=='historical':
  with p.open('a') as f:f.write(json.dumps({'timestamp':'2020-01-01T00:00:00Z','type':'event_msg','payload':{'type':'task_complete','error':{'codex_error_info':'usage_limit_exceeded'}}})+'\n')
  with p.open('a') as f:f.write(json.dumps({'timestamp':'2020-01-01T00:00:00Z','type':'event_msg','payload':{'type':'token_count','rate_limits':{'limit_id':'codex','primary':{'used_percent':100}}}})+'\n')
 time.sleep(.3)
 if mode=='chat-switch':
  p=meta(b);announce(b)
  meta(child,a)
  with log.open('a') as f:f.write('INFO session_loop{thread_id='+child+'}: codex_core::session: new\n')
  time.sleep(.3)
 event(p,'task_started');time.sleep(.15)
 if mode=='transient':
  event(p,'task_complete',error={'codex_error_info':'rate_limit_exceeded'});time.sleep(.5);sys.exit(7)
 if mode in ('quota','goal-limited','goal-paused'):
  if mode=='goal-limited':
   db=sqlite3.connect(root/'main/goals_1.sqlite');db.execute("update thread_goals set status='usage_limited'");db.commit();db.close()
  event(p,'task_complete',error={'codex_error_info':'usage_limit_exceeded'})
 else:
  if mode=='poll-threshold':
   (root/'usage.json').write_text(json.dumps(dict(alt=97,personal=10,work=10)))
  event(p,'token_count',rate_limits={'limit_id':'codex','primary':{'used_percent':90 if mode=='poll-threshold' else 97}})
  time.sleep(.6)
  (root/'work-finished').write_text('yes')
  event(p,'task_complete',error=None)
 while True:time.sleep(.1)
assert args[1]=='personal',args
assert 'resume' in args and args[args.index('resume')+1]==sid,args
assert 'ORIGINAL TASK' not in args,args
if mode not in ('quota','goal-limited','goal-paused'):assert (root/'work-finished').exists(),'Restarted during active work'
if mode=='quota':assert 'Continue the unfinished request' in args[-1]
else:assert not any('Continue the unfinished request' in arg for arg in args)
announce(sid)
if mode=='goal-limited':
 tty.setraw(0);received=b'';end=time.monotonic()+5
 while time.monotonic()<end and b'/goal resume\r' not in received:
  ready,_,_=select.select([0],[],[],.1)
  if ready:received+=os.read(0,4096)
 assert b'/goal resume\r' in received,received
 (root/'goal-resumed').write_text('yes')
print('EXACT_HANDOFF_OK',flush=True);time.sleep(.3)
'''


class PolicyTests(unittest.TestCase):
    def test_titles_are_chunked_and_resolve_exact_owned_id(self):
        parser=auto.Titles()
        wire=('\x1b]0;'+B[:29]+'...\x1b\\').encode()
        for byte in wire:parser.feed(bytes([byte]))
        self.assertEqual(parser.resolve({B,B[:-1]+'f'}),None) # Prefix collision fails closed.
        self.assertEqual(parser.resolve({B}),B)
        parser.feed(('plain chat mentions '+A).encode())
        self.assertEqual(parser.resolve({B}),B)

    def test_threshold_is_core_account_quota(self):
        report={'usage':{'allowed':True,'limitReached':False,'windows':[{'scope':'codex','usedPercent':96.9},{'scope':'review','usedPercent':100}]}}
        self.assertTrue(auto.usable(report))
        report['usage']['windows'][0]['usedPercent']=97
        self.assertFalse(auto.usable(report))

    def test_native_resume_does_not_replay_selectors_or_initial_prompt(self):
        args=['--enable','hooks','-c','hooks.Stop=[]','resume','--last','-m','model','ORIGINAL TASK']
        self.assertEqual(auto.resume_options(args),['--enable','hooks','-c','hooks.Stop=[]','-m','model'])
        self.assertEqual(auto.permission_options(['resume',A,'-s','read-only','-a','on-request','-m','model']),['resume',A,'-m','model'])


class HandoffTests(unittest.TestCase):
    def exercise(self,mode):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'bin').mkdir();(root/'main').mkdir();(root/'project').mkdir()
            fake=root/'bin/xswap';fake.write_text(FAKE);fake.chmod(0o700)
            real=root/'bin/codex';real.write_text('#!/bin/sh\nprintf "REAL_CODEX %s\\n" "$*"\n');real.chmod(0o700)
            config=root/'config.json';config.write_text(json.dumps({'real_codex':str(real),'accounts':['alt','personal','work'],'account':'alt','full_permissions':True,'poll_seconds':.1}))
            (root/'usage.json').write_text(json.dumps(dict(alt=10,personal=10,work=10)))
            if mode.startswith('goal-'):
                db=sqlite3.connect(root/'main/goals_1.sqlite')
                db.execute('create table thread_goals(thread_id text, status text, goal_id text, token_budget integer, tokens_used integer)')
                db.execute('insert into thread_goals values(?,?,?,?,?)',(A,'paused' if mode=='goal-paused' else 'active','stable-goal',12345,456))
                db.commit();db.close()
            env={**os.environ,'HOME':tmp,'PATH':str(root/'bin')+':'+os.environ['PATH'],'CODEX_AUTO_CONFIG':str(config),'FAKE_ROOT':tmp,'FAKE_MODE':mode,'TERM':'xterm-256color'}
            sibling=subprocess.Popen(['/bin/sleep','60'])
            pid,fd=pty.fork()
            if pid==0:os.execve(str(SCRIPT),[str(SCRIPT),'--','-m','test-model','-s','read-only','-a','on-request','ORIGINAL TASK'],env)
            output=b'';status=None;end=time.monotonic()+12
            try:
                while time.monotonic()<end:
                    ready,_,_=select.select([fd],[],[],.1)
                    if ready:
                        try:output+=os.read(fd,65536)
                        except OSError:pass
                    child,st=os.waitpid(pid,os.WNOHANG)
                    if child:status=st;break
                self.assertIsNotNone(status,output.decode(errors='replace'))
                self.assertIsNone(sibling.poll(),'Unrelated cmux-like process was interrupted')
                launches=[json.loads(line) for line in (root/'launches').read_text().splitlines()]
                self.assertEqual([args[1] for args in launches],['alt'] if mode=='transient' else ['alt','personal'],output)
                for args in launches:
                    self.assertIn('--dangerously-bypass-approvals-and-sandbox',args)
                    self.assertNotIn('-s',args);self.assertNotIn('-a',args)
                    self.assertIn('test-model',args)
                if mode.startswith('goal-'):
                    db=sqlite3.connect(root/'main/goals_1.sqlite')
                    self.assertEqual(db.execute('select goal_id,token_budget,tokens_used from thread_goals').fetchone(),('stable-goal',12345,456));db.close()
                if mode=='goal-limited':self.assertTrue((root/'goal-resumed').exists(),output)
                self.assertEqual(os.waitstatus_to_exitcode(status),7 if mode=='transient' else 0,output)
            finally:
                if status is None:os.kill(pid,signal.SIGTERM);os.waitpid(pid,0)
                os.close(fd);sibling.terminate();sibling.wait()

    def test_97_percent_waits_for_completed_turn(self):self.exercise('threshold')
    def test_background_quota_poll_hands_off_at_97(self):self.exercise('poll-threshold')
    def test_quota_failure_continues_exact_conversation(self):self.exercise('quota')
    def test_transient_rate_limit_does_not_rotate(self):self.exercise('transient')
    def test_historical_limits_do_not_apply_to_new_account(self):self.exercise('historical')
    def test_active_goal_is_not_duplicated(self):self.exercise('goal-active')
    def test_quota_paused_goal_uses_native_resume(self):self.exercise('goal-limited')
    def test_explicitly_paused_goal_stays_paused(self):self.exercise('goal-paused')
    def test_chat_switch_does_not_follow_subagent(self):self.exercise('chat-switch')


@unittest.skipUnless(shutil.which('zsh'), 'zsh is required for cmux shell integration')
class ShellTests(unittest.TestCase):
    def test_cmux_default_retains_native_wrapper_and_non_cmux_is_passthrough(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);root.joinpath('codex').write_text('#!/bin/sh\necho RAW_CODEX "$@"\n')
            root.joinpath('codex-auto-cli').write_text('#!/bin/sh\necho AUTO_MANAGED "$@"\n')
            root.joinpath('cmux-native').write_text('#!/bin/sh\necho NATIVE_CMUX_HOOKS\nexec "$CMUX_CUSTOM_CODEX_PATH" "$@"\n')
            for path in root.iterdir():path.chmod(0o700)
            env={**os.environ,'PATH':str(root)+':'+os.environ['PATH'],'CODEX_AUTO_BIN_DIR':tmp,'CODEX_AUTO_CMUX_WRAPPER':str(root/'cmux-native')}
            for cmux in (False,True):
                for key in ('CMUX_SURFACE_ID','CMUX_WORKSPACE_ID','CMUX_SOCKET_PATH'):env.pop(key,None)
                if cmux:env['CMUX_SURFACE_ID']='fixture'
                pid,fd=pty.fork()
                if pid==0:
                    os.execve(shutil.which('zsh'),['zsh','-f','-c','source "$1"; codex resume fixture-id','fixture',str(SCRIPT.with_suffix('.zsh'))],env)
                output=b'';status=None;end=time.monotonic()+5
                try:
                    while time.monotonic()<end:
                        ready,_,_=select.select([fd],[],[],.1)
                        if ready:
                            try:output+=os.read(fd,65536)
                            except OSError:pass
                        child,st=os.waitpid(pid,os.WNOHANG)
                        if child:status=st;break
                    self.assertIsNotNone(status,output)
                    self.assertEqual(os.waitstatus_to_exitcode(status),0,output)
                    self.assertIn(b'AUTO_MANAGED resume fixture-id' if cmux else b'RAW_CODEX resume fixture-id',output)
                    if cmux:self.assertIn(b'NATIVE_CMUX_HOOKS',output)
                finally:
                    if status is None:os.kill(pid,signal.SIGTERM);os.waitpid(pid,0)
                    os.close(fd)


if __name__=='__main__':unittest.main(verbosity=2)
