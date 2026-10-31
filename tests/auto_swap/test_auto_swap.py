import importlib.machinery
import importlib.util
import fcntl
import builtins
import contextlib
import io
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import socket
import sys
import sqlite3
import subprocess
import tempfile
import time
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[2] / 'scripts/codex-auto'
loader = importlib.machinery.SourceFileLoader('auto_swap', str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
auto = importlib.util.module_from_spec(spec)
loader.exec_module(auto)
A = '00000000-0000-7000-8000-000000000001'
B = '10000000-0000-7000-8000-000000000002'
FAKE = r'''#!/usr/bin/env python3
import datetime, json, os, pathlib, re, select, shlex, sqlite3, subprocess, sys, time, tty
root=pathlib.Path(os.environ['FAKE_ROOT']); args=sys.argv[1:]; mode=os.environ['FAKE_MODE']
accounts=[dict(alias=a,number=i+1,home=str(root/'home'),enabled=True,loginStatus='present',isDefault=i==0) for i,a in enumerate(['alt','personal','work'])]
if args[0]=='--codex-bin': args=args[2:]
if args[0]=='list': print(json.dumps({'accounts':accounts}));sys.exit()
if args[0]=='status': print(json.dumps({'defaultHome':str(root/'main')}));sys.exit()
if args[0]=='usage':
 values=json.loads((root/'usage.json').read_text())
 print(json.dumps({'accounts':[dict(number=a['number'],usage=dict(allowed=True,limitReached=False,windows=[dict(scope='codex',usedPercent=values[a['alias']])])) for a in accounts]}));sys.exit()
if args[0]=='run' and 'app-server' in args:
 for line in sys.stdin:
  message=json.loads(line)
  if message.get('id')==1:print(json.dumps({'id':1,'result':{}}),flush=True)
  if message.get('method')=='config/read':
   print(json.dumps({'id':message['id'],'result':{'config':{'hooks':{'PreModelRequest':[{'hooks':[{'type':'command','command':'printf existing-hook','timeout':5}]}]}}}}),flush=True)
 sys.exit()
assert args[0]=='run' and '--share-history' in args and '--no-daemon' in args
assert os.environ['XSWAP_STOP_CODEX']=='never' and os.getpgrp()==os.getpid()
with (root/'launches').open('a') as f:f.write(json.dumps(args)+'\n')
logdir=pathlib.Path(json.loads(next(x for x in args if x.startswith('log_dir=')).split('=',1)[1]));logdir.mkdir(parents=True,exist_ok=True)
log=logdir/'codex-tui.log'
config=next(value for value in args if value.startswith('hooks.PreModelRequest='))
assert 'existing-hook' in config, 'Existing native hooks were overwritten'
commands=re.findall(r'"command"=("(?:\\.|[^"\\])*")',config)
hook_command=json.loads(commands[-1])

a='00000000-0000-7000-8000-000000000001';b='10000000-0000-7000-8000-000000000002';child='20000000-0000-7000-8000-000000000003'
sid=b if mode=='chat-switch' else a
home=root/'home/sessions/2026/10/30';home.mkdir(parents=True,exist_ok=True)
def meta(s,parent=None):
 p=home/('rollout-'+s+'.jsonl')
 if not p.exists():p.write_text(json.dumps({'type':'session_meta','payload':{'id':s,'cwd':str(root/'project'),'parent_thread_id':parent}})+'\n')
 return p
def event(p,kind,**fields):
 if kind=='task_started':fields.setdefault('turn_id','turn-1')
 d={'timestamp':datetime.datetime.now(datetime.timezone.utc).isoformat(),'type':'event_msg','payload':dict(type=kind,**fields)}
 with p.open('a') as f:f.write(json.dumps(d)+'\n')
def checkpoint(p,turn='turn-2',wait_gate=True):
 event(p,'task_started',turn_id=turn)
 if wait_gate:
  end=time.monotonic()+20
  while time.monotonic()<end and not (logdir/'handoff-gate.json').exists():time.sleep(.02)
  assert (logdir/'handoff-gate.json').exists(), 'gate was not armed after persisted receipt/usage'
 payload=dict(hook_event_name='PreModelRequest',session_id=sid,turn_id=turn,transcript_path=str(p),cwd=str(root/'project'))
 callback=subprocess.Popen(shlex.split(hook_command),stdin=subprocess.PIPE)
 callback.communicate(json.dumps(payload).encode(),timeout=30)
 assert callback.returncode==0, callback.returncode
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
 if mode in ('quota','goal-limited','goal-paste','goal-paused'):
  if mode in ('goal-limited','goal-paste'):
   db=sqlite3.connect(root/'main/goals_1.sqlite');db.execute("update thread_goals set status='usage_limited'");db.commit();db.close()
  tty.setraw(0)
  event(p,'task_complete',error={'codex_error_info':'usage_limit_exceeded'})
  if mode!='goal-paused':
   received=b'';end=time.monotonic()+20
   while time.monotonic()<end and b'\r' not in received:
    if select.select([0],[],[],.1)[0]:received+=os.read(0,4096)
   expected=b'/goal resume' if mode in ('goal-limited','goal-paste') else b'Continue the unfinished request'
   assert received.startswith(expected) and received.endswith(b'\r'), received
   print('OLD_ACCOUNT_WAKEUP_OK',flush=True)
  checkpoint(p)
 else:
  if mode=='poll-threshold':
   (root/'usage.json').write_text(json.dumps(dict(alt=97,personal=10,work=10)))
  event(p,'token_count',rate_limits={'limit_id':'codex','primary':{'used_percent':90 if mode=='poll-threshold' else 97}})
  time.sleep(.6)
  (root/'work-finished').write_text('yes')
  if mode=='race-batch':
   timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat()
   batch=[dict(timestamp=timestamp,type='event_msg',payload=dict(type=kind,turn_id='turn-2')) for kind in ('task_complete','task_started')]
   with p.open('a') as f:f.write(''.join(json.dumps(d)+'\n' for d in batch))
   time.sleep(.7)
   (root/'second-turn-finished').write_text('yes')
   event(p,'task_complete')
  else:
   event(p,'task_complete',error=None)
  if mode=='no-checkpoint':time.sleep(.7);sys.exit(0)
  if mode=='race-delayed':
   time.sleep(.2)
   event(p,'task_started',turn_id='turn-2')
   time.sleep(.7)
   (root/'second-turn-finished').write_text('yes')
   event(p,'task_complete')
  if mode=='checkpoint-input':
   tty.setraw(0)
   print('CHECKPOINT_INPUT_NOW',flush=True)
   checkpoint(p,'turn-input-held',wait_gate=False)
   received=b'';end=time.monotonic()+5
   while time.monotonic()<end and b'\r' not in received:
    if select.select([0],[],[],.1)[0]:received+=os.read(0,4096)
   assert received==b'KEEP USER MESSAGE\r',received
   (root/'user-input-saved').write_bytes(received)
   with p.open('a') as f:
    f.write(json.dumps({'timestamp':datetime.datetime.now(datetime.timezone.utc).isoformat(), 'type':'response_item', 'payload':{'type':'message','role':'user','content':[{'type':'input_text','text':'KEEP USER MESSAGE'}]}})+'\n')
   checkpoint(p,'turn-user-ack')
  else:
   checkpoint(p,'turn-3' if mode.startswith('race-') else 'turn-2')
 while True:
  time.sleep(.1)
  checkpoint(p,'retry-checkpoint')
assert args[1]=='personal',args
assert 'resume' in args and args[args.index('resume')+1]==sid,args
assert 'ORIGINAL TASK' not in args,args
if mode not in ('quota','goal-limited','goal-paste','goal-paused'):assert (root/'work-finished').exists(),'Restarted during active work'
if mode in ('quota','threshold','poll-threshold','historical','chat-switch','race-batch','race-delayed','checkpoint-input'):assert 'Continue the unfinished request' in args[-1]
else:assert not any('Continue the unfinished request' in arg for arg in args)
if mode.startswith('race-'):assert (root/'second-turn-finished').exists(), 'Interrupted the next active turn'
if mode=='checkpoint-input':assert (root/'user-input-saved').read_bytes()==b'KEEP USER MESSAGE\r'
announce(sid)
if mode in ('goal-limited','goal-paste'):
 tty.setraw(0);received=b'';end=time.monotonic()+5
 while time.monotonic()<end and b'/goal resume\r' not in received:
  ready,_,_=select.select([0],[],[],.1)
  if ready:
   received+=os.read(0,4096)
   if mode=='goal-paste' and received==b'/goal resume':print('PASTE_NOW',flush=True)
 assert received.split(b'\r',1)[0]==b'/goal resume',received
 if mode=='goal-paste':
  while time.monotonic()<end and b'\x1b[201~' not in received:
   if select.select([0],[],[],.1)[0]:received+=os.read(0,4096)
  assert received==b'/goal resume\r\x1b[200~USER DRAFT\x1b[201~',received
 (root/'goal-resumed').write_text('yes')
print('EXACT_HANDOFF_OK',flush=True);time.sleep(.3)
'''


class PolicyTests(unittest.TestCase):
    @staticmethod
    def user_receipt(text, timestamp='1970-01-01T00:01:41Z'):
        return {'type': 'response_item', 'timestamp': timestamp,
                'payload': {'type': 'message', 'role': 'user',
                            'content': [{'type': 'input_text', 'text': text}]}}

    def test_enter_needs_matching_persisted_message_before_handoff(self):
        fence = auto.InputFence()
        fence.received(b'new request\r', now=100)
        self.assertFalse(fence.permits(b''))
        fence.observe([self.user_receipt('new request', '1970-01-01T00:01:39Z')])
        self.assertFalse(fence.permits(b''))
        fence.observe([self.user_receipt('unrelated queued request')])
        self.assertFalse(fence.permits(b''))
        fence.observe([self.user_receipt('new request')])
        self.assertFalse(fence.permits(b'not yet written'))
        self.assertTrue(fence.permits(b''))

    def test_each_submission_needs_its_own_receipt(self):
        fence = auto.InputFence()
        fence.received(b'one\r', now=100)
        fence.received(b'\x1b[200~two\nlines\x1b[201~\r', now=100)
        fence.observe([self.user_receipt('one')])
        self.assertFalse(fence.permits(b''))
        fence.observe([self.user_receipt('two\nlines')])
        self.assertTrue(fence.permits(b''))

    def test_non_submitting_input_does_not_wait_for_a_nonexistent_turn(self):
        for data in [b'\x1b]10;rgb:ffff/0000/aaaa\x1b\\', b'\x1b[12;34R',
                     b'\x1b[A', b'\x03', b'cancelled draft\x03', b'\r']:
            fence = auto.InputFence()
            fence.received(data)
            self.assertTrue(fence.permits(b''))

    def test_generated_turn_and_context_do_not_acknowledge_user_input(self):
        fence = auto.InputFence()
        fence.received(b'KEEP USER MESSAGE\r', now=100)
        fence.observe([{'type': 'event_msg', 'timestamp': '1970-01-01T00:01:41Z',
                        'payload': {'type': 'task_started', 'turn_id': 'generated'}},
                       self.user_receipt('<codex_internal_context>continue goal</codex_internal_context>')])
        self.assertFalse(fence.permits(b''))
        fence.observe([self.user_receipt('KEEP USER MESSAGE')])
        self.assertTrue(fence.permits(b''))

    def test_unknown_composer_edits_cannot_be_acknowledged_by_a_guess(self):
        fence = auto.InputFence()
        fence.received(b'word\x1b[Dedited\r', now=100)
        fence.observe([self.user_receipt('wordedited')])
        self.assertFalse(fence.permits(b''))

    def test_identical_generated_continuation_cannot_acknowledge_user_submission(self):
        fence = auto.InputFence()
        fence.expect_generated(auto.CONTINUE)
        fence.received(auto.CONTINUE.encode()+b'\r', now=100)
        fence.observe([self.user_receipt(auto.CONTINUE)])
        self.assertFalse(fence.permits(b''))
        fence.observe([self.user_receipt(auto.CONTINUE)])
        self.assertTrue(fence.permits(b''))

    def test_modified_enter_does_not_falsely_clear_a_multiline_draft(self):
        state = auto.InputState()
        state.feed(b'KEEP DRAFT\x1b[13;2u')
        self.assertTrue(state.dirty)
        self.assertTrue(state.uncertain)
        self.assertEqual(state.submitted, [])

    def test_control_keys_cannot_falsely_clear_an_unpersisted_draft(self):
        for control in (b'\x03', b'\x04', b'\x15', b'\x1b[D\x15'):
            state = auto.InputState()
            state.feed(b'KEEP DRAFT' + control)
            self.assertTrue(state.dirty, control)
            self.assertTrue(state.uncertain, control)
            fence = auto.InputFence()
            fence.received(b'KEEP DRAFT' + control + b'\r', now=100)
            fence.observe([self.user_receipt('KEEP DRAFT')])
            self.assertFalse(fence.permits(b''), control)

    def test_basic_editing_and_paste_bytes_match_the_actual_user_message(self):
        fence = auto.InputFence()
        fence.received('🧪'.encode()+b'\x7f', now=100)
        fence.received(b'\x1b[200~literal\x1b[12;34R\x1b[201~\r', now=100)
        fence.observe([self.user_receipt('literal\x1b[12;34R')])
        self.assertTrue(fence.permits(b''))

    def test_python39_sqlite_home_accepts_toml_comments_and_string_escapes(self):
        original_import = builtins.__import__
        def without_tomllib(name, *args, **kwargs):
            if name == 'tomllib':
                raise ImportError('exercise Python 3.9 fallback')
            return original_import(name, *args, **kwargs)
        examples = [(r'"custom#state" # trailing comment', 'custom#state'),
                    ("'literal#state' # trailing comment", 'literal#state'),
                    (r'"unicode-\U0001F9EA" # comment', 'unicode-🧪'),
                    (r'"escaped-\u0061" # comment', 'escaped-a')]
        with tempfile.TemporaryDirectory() as tmp, mock.patch('builtins.__import__', side_effect=without_tomllib):
            home = Path(tmp)
            for scalar, expected in examples:
                (home/'config.toml').write_text('sqlite_home = ' + scalar + '\n[other]\n')
                self.assertEqual(auto.sqlite_home(home), (home/expected).resolve())
            for scalar in ['"unterminated # comment', '"ok" extra', r'"bad-\q"', r'"bad-\uD800"']:
                (home/'config.toml').write_text('sqlite_home = ' + scalar)
                with self.subTest(scalar=scalar), self.assertRaises(ValueError):
                    auto.sqlite_home(home)

    def test_only_terminal_replies_bypass_hold_and_paste_payload_is_literal(self):
        hold = auto.InputHold()
        self.assertEqual(hold.append(b'\x1b]10;rgb:ffff'), b'')
        self.assertEqual(hold.append(b'/0000/aaaa\x1b\\'),
                         b'\x1b]10;rgb:ffff/0000/aaaa\x1b\\')
        self.assertFalse(hold.data)
        self.assertEqual(hold.append(b'\x1b[12;34R'), b'\x1b[12;34R')
        draft = b'\x1b[200~USER\x1b[12;34R\x1b[201~'
        for byte in draft:
            self.assertEqual(hold.append(bytes([byte])), b'')
        self.assertEqual(hold.release(), draft)

    def test_transport_restores_flags_when_output_is_stalled(self):
        left, right = socket.socketpair()
        reader, writer = os.pipe()
        original = fcntl.fcntl(writer, fcntl.F_GETFL)
        pump = auto.TerminalPump(left.fileno(), writer)
        try:
            pump.queue(writer, b'x' * 262144)
            started = time.monotonic()
            pump.close()
            self.assertLess(time.monotonic() - started, 2)
            self.assertEqual(fcntl.fcntl(writer, fcntl.F_GETFL) & os.O_NONBLOCK,
                             original & os.O_NONBLOCK)
            self.assertTrue(pump.pending[writer])
        finally:
            left.close(); right.close(); os.close(reader); os.close(writer)

    def test_permission_flags_do_not_consume_literal_prompts_or_other_values(self):
        args = ['-c', '-sandbox=value', '-m', '-a-model', '-s', 'read-only',
                '--', '-summarize this repository', '-a', 'literal']
        self.assertEqual(auto.permission_options(args),
                         ['-c', '-sandbox=value', '-m', '-a-model',
                          '--', '-summarize this repository', '-a', 'literal'])

    def test_transport_keeps_partial_writes_and_retries_backpressure(self):
        left, right = socket.socketpair()
        reader, writer = os.pipe()
        pump = auto.TerminalPump(left.fileno(), writer)
        try:
            pump.queue(writer, b'abcdef')
            with mock.patch.object(auto.os, 'write', side_effect=[2, BlockingIOError(), 4]):
                pump.flush(writer)
                self.assertEqual(pump.pending[writer], b'cdef')
                pump.flush(writer)
                self.assertEqual(pump.pending[writer], b'cdef')
                pump.flush(writer)
                self.assertFalse(pump.pending[writer])
        finally:
            pump.close()
            left.close(); right.close(); os.close(reader); os.close(writer)

    def test_transport_saturation_does_not_block_opposite_direction(self):
        left, right = socket.socketpair()
        left.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
        reader, writer = os.pipe()
        os.set_blocking(reader, False)
        right.setblocking(False)
        pump = auto.TerminalPump(left.fileno(), writer)
        payload = b'x' * 262144
        try:
            pump.queue(writer, payload)
            pump.queue(left.fileno(), payload)
            # Neither consumer drains: repeated polling must still return.
            started = time.monotonic()
            for _ in range(10):
                pump.poll(.001)
            self.assertLess(time.monotonic() - started, 1)
            self.assertTrue(pump.pending[writer])
            self.assertTrue(pump.pending[left.fileno()])
            output, child_input = bytearray(), bytearray()
            end = time.monotonic() + 5
            while time.monotonic() < end and (len(output) < len(payload) or len(child_input) < len(payload)):
                pump.poll(.001)
                for fd, target in [(reader, output), (right.fileno(), child_input)]:
                    try:
                        target.extend(os.read(fd, 65536))
                    except BlockingIOError:
                        pass
            self.assertEqual(output, payload)
            self.assertEqual(child_input, payload)
        finally:
            pump.close()
            left.close(); right.close(); os.close(reader); os.close(writer)

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

    def test_readiness_hold_has_a_per_input_deadline_and_preserves_bytes(self):
        hold=auto.InputHold()
        self.assertFalse(hold.expired(now=10000))
        hold.append(b'draft',now=10000)
        hold.append(b' continued',now=10010)
        self.assertFalse(hold.expired(now=10014.9))
        self.assertTrue(hold.expired(now=10015))
        self.assertEqual(hold.release(),b'draft continued')
        self.assertFalse(hold.expired(now=20000))
        hold.append(b'new draft',now=20000)
        self.assertFalse(hold.expired(now=20001))
        self.assertEqual(hold.release(),b'new draft')

    def test_pasted_multiline_draft_is_not_a_submit(self):
        state=auto.InputState()
        state.feed(b'\x1b]10;rgb:ffff/ffff/ffff\x1b\\')
        self.assertFalse(state.dirty)
        for chunk in (b'\x1b[20',b'0~unsent\ntext',b'\x1b[201~'):
            state.feed(chunk)
        self.assertTrue(state.dirty)
        state.feed(b'\r')
        self.assertFalse(state.dirty)

    def test_attach_does_not_skip_concurrently_appended_event(self):
        with tempfile.TemporaryDirectory() as tmp:
            home=Path(tmp);directory=home/'sessions/2026/10/30';directory.mkdir(parents=True)
            path=directory/('rollout-'+A+'.jsonl')
            path.write_text(json.dumps({'type':'session_meta','payload':{'id':A}})+'\n')
            original=auto.Tail.__init__
            def append_before_tail(tail,*args,**kwargs):
                with path.open('a') as stream:
                    stream.write(json.dumps({'type':'event_msg','payload':{'type':'task_started','turn_id':'concurrent-turn'}})+'\n')
                original(tail,*args,**kwargs)
            with mock.patch.object(auto.Tail,'__init__',append_before_tail):
                conversation=auto.Conversation(A,home)
                events=conversation.events()
            self.assertEqual(conversation.turn,'concurrent-turn')
            self.assertEqual(len(events),1)

    def test_unreadable_goal_state_is_not_no_goal(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory=Path(tmp)
            (directory/'goals_1.sqlite').write_bytes(b'invalid database')
            self.assertEqual(auto.goal_status(directory,A),'unknown')

    def test_checkpoint_requires_current_nonce_session_and_live_lease(self):
        with tempfile.TemporaryDirectory() as tmp:
            gate=auto.HandoffGate(Path(tmp));gate.arm(A)
            record=dict(session=A,nonce='wrong',pid=os.getpid(),expires=time.time()+30,turn='turn')
            gate.ready.write_text(json.dumps(record));self.assertFalse(gate.held(A))
            record['nonce']=gate.request['nonce'];record['session']=B
            gate.ready.write_text(json.dumps(record));self.assertFalse(gate.held(A))
            record['session']=A;record['expires']=time.time()-1
            gate.ready.write_text(json.dumps(record));self.assertFalse(gate.held(A))

    def test_checkpoint_lease_ignores_process_local_monotonic_origin(self):
        with tempfile.TemporaryDirectory() as tmp:
            gate = auto.HandoffGate(Path(tmp)); gate.arm(A)
            record = dict(session=A, nonce=gate.request['nonce'], pid=os.getpid(),
                          expires=time.time()+45, turn='turn')
            gate.ready.write_text(json.dumps(record))
            with mock.patch.object(auto.time, 'monotonic', return_value=10**12):
                self.assertEqual(gate.held(A), record)
            record['expires'] = time.time()-1
            gate.ready.write_text(json.dumps(record))
            with mock.patch.object(auto.time, 'monotonic', return_value=0):
                self.assertFalse(gate.held(A))

    def test_callback_tolerates_gate_cancelled_after_existence_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            (directory/'handoff-gate.json').write_text('{}')
            original = Path.read_text
            def cancelled(path, *args, **kwargs):
                if path.name == 'handoff-gate.json':
                    raise FileNotFoundError('cancelled while reading payload')
                return original(path, *args, **kwargs)
            class Stdin:
                buffer = __import__('io').BytesIO(b'{"hook_event_name":"PreModelRequest"}')
            with mock.patch.object(auto.sys, 'stdin', Stdin()), mock.patch.object(Path, 'read_text', cancelled):
                self.assertEqual(auto.checkpoint_hook(directory), 0)
            self.assertFalse((directory/'handoff-ready.json').exists())

    def test_native_callback_cannot_release_until_gate_is_cancelled(self):
        with tempfile.TemporaryDirectory() as tmp:
            gate=auto.HandoffGate(Path(tmp));gate.arm(A)
            callback=subprocess.Popen([sys.executable,str(SCRIPT),'--checkpoint',tmp],stdin=subprocess.PIPE)
            callback.stdin.write(json.dumps(dict(session_id=A,hook_event_name='PreModelRequest',turn_id='turn')).encode());callback.stdin.close()
            try:
                end=time.monotonic()+30
                while time.monotonic()<end and not gate.held(A):time.sleep(.02)
                self.assertTrue(gate.held(A), 'callback=%r ready_file=%r' % (callback.poll(), gate.ready.exists()));self.assertIsNone(callback.poll())
                gate.cancel();self.assertEqual(callback.wait(timeout=3),0)
            finally:
                if callback.poll() is None:callback.kill();callback.wait()

    def test_subagent_callback_does_not_park_root(self):
        with tempfile.TemporaryDirectory() as tmp:
            gate=auto.HandoffGate(Path(tmp));gate.arm(A)
            payload=dict(session_id=A,hook_event_name='PreModelRequest',agent_id='child',turn_id='turn')
            result=subprocess.run([sys.executable,str(SCRIPT),'--checkpoint',tmp],input=json.dumps(payload).encode(),timeout=3)
            self.assertEqual(result.returncode,0);self.assertFalse(gate.held(A))


class ConfigurationTests(unittest.TestCase):
    def configure(self, path, config, arguments):
        with contextlib.redirect_stdout(io.StringIO()) as output:
            auto.configure(path, config, arguments)
        return output.getvalue()

    def test_remaining_cutoff_preserves_unrelated_settings_and_permissions(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            old = {'full_permissions': True, 'real_codex': '/custom/codex',
                   'future_setting': {'secret': 'private'}, 'accounts': ['one', 'two']}
            path.write_text(json.dumps(old))
            text = self.configure(path, old, ['--remaining-percent', '5', '--poll-seconds', '12',
                                              '--cooldown-seconds', '60'])
            saved = json.loads(path.read_text())
            self.assertEqual(saved, {**old, 'threshold': 95, 'poll_seconds': 12, 'cooldown_seconds': 60})
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertNotIn('private', text)
            self.assertIn('Running sessions are unchanged', text)

    def test_show_does_not_create_files_or_launch_codex(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'missing/config.json'
            with mock.patch.dict(os.environ, {'CODEX_AUTO_CONFIG': str(path)}), \
                    mock.patch.object(sys, 'argv', ['codex-auto', 'configure', '--show']), \
                    mock.patch.object(auto, 'real_binary', side_effect=AssertionError('must not launch')):
                with contextlib.redirect_stdout(io.StringIO()) as output:
                    self.assertEqual(auto.main(), 0)
            settings = json.loads(output.getvalue())
            self.assertEqual(settings['threshold'], 97)
            self.assertEqual(settings['remaining_percent'], 3)
            self.assertEqual(settings['cooldown_seconds'], 300)
            self.assertFalse(path.parent.exists())

    def test_invalid_values_leave_config_intact(self):
        bad = [['--threshold', 'nan'], ['--threshold', '101'], ['--remaining-percent', '100'],
               ['--poll-seconds', '0'], ['--cooldown-seconds', '-1'],
               ['--accounts', 'a,a'], ['--accounts', 'a,'], ['--account', 'absent']]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            old = {'accounts': ['a', 'b'], 'account': 'a'}
            path.write_text(json.dumps(old))
            original = path.read_bytes()
            for arguments in bad:
                with self.subTest(arguments=arguments), self.assertRaises(RuntimeError):
                    self.configure(path, old, arguments)
                self.assertEqual(path.read_bytes(), original)

    def test_explicit_permission_toggle_and_account_order(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            self.configure(path, {}, ['--accounts', 'b,a', '--account', 'b', '--full-permissions'])
            saved = json.loads(path.read_text())
            self.assertEqual(saved['accounts'], ['b', 'a'])
            self.assertTrue(saved['full_permissions'])
            self.configure(path, saved, ['--no-full-permissions'])
            self.assertFalse(json.loads(path.read_text())['full_permissions'])

    def test_concurrent_writers_reread_under_a_stable_lock(self):
        worker = """import importlib.machinery, importlib.util, json, pathlib, sys
loader=importlib.machinery.SourceFileLoader('config_worker',sys.argv[1])
spec=importlib.util.spec_from_loader(loader.name,loader)
module=importlib.util.module_from_spec(spec);loader.exec_module(module)
snapshot=json.loads(pathlib.Path(sys.argv[2]).read_text())
print('READY',flush=True)
sys.stdin.readline()
module.configure(pathlib.Path(sys.argv[2]),snapshot,sys.argv[3:])
"""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'config.json'
            path.write_text(json.dumps({'threshold': 97, 'poll_seconds': 30, 'full_permissions': True}))
            lock_path = path.with_name(path.name+'.lock')
            processes = []
            try:
                with lock_path.open('a') as lock:
                    fcntl.flock(lock, fcntl.LOCK_EX)
                    for arguments in (['--threshold', '90'], ['--poll-seconds', '12']):
                        proc = subprocess.Popen([sys.executable, '-c', worker, str(SCRIPT), str(path)] + arguments,
                                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                                text=True)
                        processes.append(proc)
                    for proc in processes:
                        self.assertTrue(select.select([proc.stdout], [], [], 10)[0])
                        self.assertEqual(proc.stdout.readline().strip(), 'READY')
                    for proc in processes:
                        proc.stdin.write('GO\n');proc.stdin.flush()
                    fcntl.flock(lock, fcntl.LOCK_UN)
                for proc in processes:
                    out, err = proc.communicate(timeout=10)
                    self.assertEqual(proc.returncode, 0, out+err)
                saved = json.loads(path.read_text())
                self.assertEqual(saved, {'threshold': 90, 'poll_seconds': 12, 'full_permissions': True})
                self.assertTrue(lock_path.exists())
            finally:
                for proc in processes:
                    if proc.poll() is None:
                        proc.kill();proc.wait()

    def test_atomic_write_failure_preserves_old_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'config.json'
            path.write_text('{"threshold": 97}')
            with mock.patch.object(auto.os, 'replace', side_effect=OSError('write failure')):
                with self.assertRaises(OSError):
                    self.configure(path, {'threshold': 97}, ['--threshold', '90'])
            self.assertEqual(path.read_text(), '{"threshold": 97}')
            self.assertEqual(set(Path(tmp).iterdir()), {path, path.with_name(path.name+'.lock')})

    def test_numeric_and_boolean_configuration_types_are_strict(self):
        for config in ({'threshold': True}, {'poll_seconds': '30'}, {'cooldown_seconds': float('inf')},
                       {'full_permissions': 'false'}):
            with self.subTest(config=config), self.assertRaises(RuntimeError):
                auto.rotation_settings(config)


class HandoffTests(unittest.TestCase):
    def exercise(self,mode):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);(root/'bin').mkdir();(root/'main').mkdir();(root/'project').mkdir()
            fake=root/'bin/xswap';fake.write_text(FAKE.replace('#!/usr/bin/env python3', '#!'+sys.executable, 1));fake.chmod(0o700)
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
            if pid==0:os.execve(sys.executable,[sys.executable,str(SCRIPT),'--','-m','test-model','-s','read-only','-a','on-request','ORIGINAL TASK'],env)
            output=b'';status=None;paste_sent=False;checkpoint_input_sent=False;end=time.monotonic()+90
            try:
                while time.monotonic()<end:
                    ready,_,_=select.select([fd],[],[],.1)
                    if ready:
                        try:output+=os.read(fd,65536)
                        except OSError:pass
                        if mode=='goal-paste' and b'PASTE_NOW' in output and not paste_sent:
                            os.write(fd,b'\x1b[200~USER DRAFT\x1b[201~')
                            paste_sent=True
                    if mode=='checkpoint-input' and b'CHECKPOINT_INPUT_NOW' in output and not checkpoint_input_sent:
                        os.write(fd,b'KEEP USER MESSAGE\r')
                        checkpoint_input_sent=True
                    child,st=os.waitpid(pid,os.WNOHANG)
                    if child:status=st;break
                self.assertIsNotNone(status,output.decode(errors='replace'))
                self.assertIsNone(sibling.poll(),'Unrelated cmux-like process was interrupted')
                launches=[json.loads(line) for line in (root/'launches').read_text().splitlines()]
                self.assertEqual([args[1] for args in launches],['alt'] if mode in ('transient','no-checkpoint') else ['alt','personal'],output)
                for args in launches:
                    self.assertIn('--dangerously-bypass-approvals-and-sandbox',args)
                    self.assertNotIn('-s',args);self.assertNotIn('-a',args)
                    self.assertIn('test-model',args)
                if mode.startswith('goal-'):
                    db=sqlite3.connect(root/'main/goals_1.sqlite')
                    self.assertEqual(db.execute('select goal_id,token_budget,tokens_used from thread_goals').fetchone(),('stable-goal',12345,456));db.close()
                if mode in ('goal-limited','goal-paste'):self.assertTrue((root/'goal-resumed').exists(),output)
                if mode in ('quota','goal-limited','goal-paste'):self.assertIn(b'OLD_ACCOUNT_WAKEUP_OK',output)
                self.assertEqual(os.waitstatus_to_exitcode(status),7 if mode=='transient' else 0,output)
            finally:
                if status is None:os.kill(pid,signal.SIGTERM);os.waitpid(pid,0)
                os.close(fd);sibling.terminate();sibling.wait()

    def test_submitted_input_at_checkpoint_is_saved_before_handoff(self):self.exercise('checkpoint-input')

    def test_97_percent_waits_for_completed_turn(self):self.exercise('threshold')
    def test_completion_followed_by_start_in_same_read(self):self.exercise('race-batch')
    def test_new_turn_after_observed_completion(self):self.exercise('race-delayed')
    def test_missing_or_untrusted_hook_never_forces_handoff(self):self.exercise('no-checkpoint')
    def test_background_quota_poll_hands_off_at_97(self):self.exercise('poll-threshold')
    def test_quota_failure_continues_exact_conversation(self):self.exercise('quota')
    def test_transient_rate_limit_does_not_rotate(self):self.exercise('transient')
    def test_historical_limits_do_not_apply_to_new_account(self):self.exercise('historical')
    def test_active_goal_is_not_duplicated(self):self.exercise('goal-active')
    def test_paste_during_goal_resume_is_preserved_after_submission(self):self.exercise('goal-paste')

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
