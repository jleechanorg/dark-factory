# Executor reference — first READY canary

Planning artifact. These commands are for future authorized execution on jeff-ubuntu from the factory-assigned repository root. The operator does not run the coding steps. The artifact directory is `/home/jleechan/roadmap/af-first-ready-20260913`; execution receipts go under its `evidence` directory. Before any dependent step, require all referenced files to exist; absence means STOP, report the named missing file, and do not substitute another path or task.

## Invoking a proof

For a section named HOLDS, the exact command is:

```bash
python3 -c 'from pathlib import Path; t=Path("/home/jleechan/roadmap/af-first-ready-20260913/execution-reference.md").read_text(); exec(t.split("## HOLDS\n",1)[1].split("```python\n",1)[1].split("```",1)[0])'
```

Use the same command with the literal section name P0, C4-RED, C4-GREEN, OWNERSHIP or E2E in place of HOLDS. Every proof prints its own PASS/FAIL marker and exits nonzero on failure. The reference and baseline must match the published SHA256SUMS manifest. The implementation worker does not author or edit verification receipts.

## P0

This checks admission receipts; it is not a substitute for the independent reproduction described immediately below. Missing receipts are expected before prerequisite repair. Do not write true booleans to get past the check.

```python
import json, pathlib, subprocess, sys, re
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913')
    d=json.loads((root/'evidence/prerequisites.json').read_text())
    release=subprocess.check_output(['systemctl','--user','show','ai.dark-factory.daemon.service','-p','ExecStart','--value'],text=True).strip()
    assert d['exec_start']==release
    assert d['engine']=='strongdm-go'
    assert set(d['edges'])=={'router','coder','reviewer','fallback'}
    for edge in d['edges'].values():
        assert edge['validated_before_spawn'] is True and edge['invalid_scope_spawns_zero_children'] is True
        assert edge['runtime_corroborated'] is True
    assert d['independent_reproduction'] is True
    assert d['verifier_family'] != d['implementer_family']
    assert d['bootstrap_supported'] is True
    assert d['administrative_sequence_supported'] is True
    assert d['beads_preflight_verified'] is True
    assert d['implementer_cli'] and d['implementer_model']
    assert d['source_head']==re.search(r'/releases/([0-9a-f]{40})/',release).group(1)
    print('PASS P0')
except Exception as exc:
    print('FAIL P0',type(exc).__name__,str(exc));sys.exit(1)
```

Only the independent factory reviewer may produce prerequisites.json after re-executing the actual account-boundary tests and observing a compliant real launch. The independent reviewer also records beads_preflight_verified only after rerunning the canonical exact-store where/sync-status/doctor preflight, recording all output and the disposition of warnings. It also records the actual implementer_cli and implementer_model for accurate commit attribution and administrative_sequence_supported only after verifying contract readability, an independent RED review, and worker continuation within prompt limits. For each of router, coder, reviewer and fallback it records the complete executed argv with credential values omitted, source symbol and committed SHA, child identity, validation-before-spawn evidence, and the invalid-scope zero-child result in a separate raw transcript. The JSON fields are an index into that transcript, not evidence by themselves. It must query the actual factory adapter and supported Go AO command, not infer Go routing from `ai.dark-factory.ao.service`. If this cannot be reproduced, record `bootstrap_supported=false` and the exact failed edge/command in a blocker report; P0 remains FAIL. The assessment lane may finish its report in this state, but TEST/IMPL admission remains blocked. Existing repair owners are i92jy (account scope) and btlc0 (harness); the operator must not hand-fix either from a canary lane.

## C4-RED

Run after the specified tests are added, before changing production. Preserve complete command output. Compilation errors and zero selected tests are not RED. The two non-regression safety cases must already pass; only the permanent-collision telemetry assertion fails.

```python
import pathlib, subprocess, sys, time, json
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence');root.mkdir(parents=True,exist_ok=True)
    sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    stamp=str(time.time_ns())
    dirty=subprocess.run(['git','diff','--quiet']).returncode!=0
    print(json.dumps({'head':sha,'uncommitted_changes':dirty,'phase':'preliminary' if dirty else 'committed'}))
    p=subprocess.run(['cargo','test','--manifest-path','daemon/Cargo.toml','--test','tick_integration','register_branch','--','--nocapture'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
    (root/('red-'+sha+'-'+stamp+'.log')).write_text(json.dumps({'head':sha,'uncommitted_changes':dirty})+'\n'+p.stdout)
    names=['register_branch_non_transient_collision_emits_parked_human_held_after_durable_save','register_branch_transient_failure_remains_retryable_without_parking','register_branch_park_save_failure_suppresses_park_telemetry']
    assert p.returncode!=0 and 'error[E' not in p.stdout and 'could not compile' not in p.stdout
    assert all(n in p.stdout for n in names)
    assert 'C4_EXPECT_DURABLE_PARK_TELEMETRY' in p.stdout
    assert all(('test '+n+' ... ok') in p.stdout for n in names[1:])
    print('PASS C4-RED')
except Exception as exc:
    print('FAIL C4-RED',type(exc).__name__,str(exc));sys.exit(1)
```

## C4-GREEN

```python
import pathlib, subprocess, sys, time, json
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence');root.mkdir(parents=True,exist_ok=True)
    sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    stamp=str(time.time_ns())
    dirty=subprocess.run(['git','diff','--quiet']).returncode!=0
    print(json.dumps({'head':sha,'uncommitted_changes':dirty,'phase':'preliminary' if dirty else 'committed'}))
    for target,selector in [('--lib','register_branch'),('--test','register_branch')]:
        cmd=['cargo','test','--manifest-path','daemon/Cargo.toml',target]
        if target=='--test':cmd+=['tick_integration']
        cmd+=[selector,'--','--nocapture']
        p=subprocess.run(cmd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
        (root/('green-'+sha+'-'+stamp+'-'+target[2:]+'.log')).write_text(p.stdout)
        assert p.returncode==0 and 'running 0 tests' not in p.stdout
        if target=='--test':
            for n in ['register_branch_non_transient_collision_emits_parked_human_held_after_durable_save','register_branch_transient_failure_remains_retryable_without_parking','register_branch_park_save_failure_suppresses_park_telemetry']:
                assert ('test '+n+' ... ok') in p.stdout
    p=subprocess.run(['cargo','test','--manifest-path','daemon/Cargo.toml','--test','tick_integration'],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
    (root/('integration-'+sha+'-'+stamp+'.log')).write_text(p.stdout)
    assert p.returncode==0 and 'running 0 tests' not in p.stdout
    print('PASS C4-GREEN')
except Exception as exc:
    print('FAIL C4-GREEN',type(exc).__name__,str(exc));sys.exit(1)
```

## TEST-OWNERSHIP

```python
import json, pathlib, subprocess, sys
try:
    d=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/revisions.json').read_text())
    for key in ['base_sha','test_sha']:
        assert len(d[key])==40
        subprocess.run(['git','cat-file','-e',d[key]+'^{commit}'],check=True)
    subprocess.run(['git','merge-base','--is-ancestor',d['base_sha'],d['test_sha']],check=True)
    files=set(subprocess.check_output(['git','diff','--name-only',d['base_sha'],d['test_sha']],text=True).splitlines())
    assert files and files <= {'daemon/tests/tick_integration.rs','daemon/tests/common/mod.rs'}
    assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==d['test_sha']
    subprocess.run(['git','diff','--quiet'],check=True)
    subprocess.run(['git','diff','--cached','--quiet'],check=True)
    print('PASS TEST-OWNERSHIP')
except Exception as exc:
    print('FAIL TEST-OWNERSHIP',type(exc).__name__,str(exc));sys.exit(1)
```

## OWNERSHIP

The factory worker writes `evidence/revisions.json` after each committed unit using `git rev-parse HEAD` for the current value. Keys are `base_sha` (checkout before TEST edits), `test_sha` (committed RED), and `impl_sha` (committed GREEN). The independent verifier resolves each commit and recomputes the diff; it does not trust an author's claimed file list.

```python
import json, pathlib, subprocess, sys
try:
    d=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/revisions.json').read_text())
    for key in ['base_sha','test_sha','impl_sha']:
        assert len(d[key])==40
        subprocess.run(['git','cat-file','-e',d[key]+'^{commit}'],check=True)
    subprocess.run(['git','merge-base','--is-ancestor',d['base_sha'],d['test_sha']],check=True)
    subprocess.run(['git','merge-base','--is-ancestor',d['test_sha'],d['impl_sha']],check=True)
    def files(a,b):
        return set(subprocess.check_output(['git','diff','--name-only',a,b],text=True).splitlines())
    assert files(d['base_sha'],d['test_sha']) and files(d['base_sha'],d['test_sha']) <= {'daemon/tests/tick_integration.rs','daemon/tests/common/mod.rs'}
    assert files(d['test_sha'],d['impl_sha'])=={'daemon/src/tick.rs'}
    subprocess.run(['git','diff','--quiet'],check=True)
    subprocess.run(['git','diff','--cached','--quiet'],check=True)
    assert subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()==d['impl_sha']
    print('PASS OWNERSHIP')
except Exception as exc:
    print('FAIL OWNERSHIP',type(exc).__name__,str(exc));sys.exit(1)
```

## HOLDS

```python
import json, pathlib, sqlite3, sys
try:
    baseline=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/held-baseline.json').read_text())
    c=sqlite3.connect('file:/home/jleechan/.dark-factory/daemon-cxdb.sqlite?mode=ro',uri=True)
    now=[list(r) for r in c.execute("select bead_id,state,park_reason from bead_overlay where park_reason='operator_scope_hold_20260907_three_prs' order by bead_id")]
    assert len(baseline)==745 and now==baseline
    print('PASS HOLDS')
except Exception as exc:
    print('FAIL HOLDS',type(exc).__name__,str(exc));sys.exit(1)
```

## E2E

```python
import json, pathlib, sqlite3, subprocess, sys
try:
    c=sqlite3.connect('file:/home/jleechan/.dark-factory/daemon-cxdb.sqlite?mode=ro',uri=True)
    row=c.execute("select state,pr_number,branch from bead_overlay where bead_id='dark-factory-c4zhq'").fetchone()
    assert row and row[0]=='READY' and row[1] and row[2]
    pr=json.loads(subprocess.check_output(['gh','pr','view',str(row[1]),'--repo','jleechanorg/dark-factory','--json','headRefOid,headRefName,state,mergedAt,isDraft,mergeable,statusCheckRollup'],text=True))
    assert pr['state']=='OPEN' and pr['mergedAt'] is None and pr['isDraft'] is True
    assert pr['headRefName']==row[2] and pr['mergeable']=='MERGEABLE'
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence')
    evidence=json.loads((root/'independent-e2e.json').read_text())
    assert evidence['head_sha']==pr['headRefOid']
    assert evidence['bead_id']=='dark-factory-c4zhq'
    assert evidence['actual_worker_created_commit'] is True and evidence['independent_reexecution'] is True
    assert evidence['lifecycle']==['QUEUED','DISPATCHING','DISPATCHED','ATTESTED','READY']
    required={'ci_green','no_conflicts','coderabbit','bugbot','comments_resolved','evidence_review','skeptic','vacuous_red_green'}
    assert set(evidence['gates'])==required
    assert all(v=='pass' for v in evidence['gates'].values())
    assert evidence['task_dispatched_observed'] is True
    print(json.dumps(pr['statusCheckRollup'],sort_keys=True))
    print('PASS E2E')
except Exception as exc:
    print('FAIL E2E',type(exc).__name__,str(exc));sys.exit(1)
```

The independent reviewer must create independent-e2e.json only after fresh source-system queries: AO session/worktree records and raw transcript, Git commit/push ancestry and remote SHA, SQLite overlay, and the latest `GATE_ASSESSMENT` at the live PR head. Retain raw records in evidence with SHA256SUMS. `TASK_DISPATCHED` is the real success event. Parse telemetry as bytes/JSON lines; preserve and report malformed/NUL-containing records rather than using grep's binary-file result as absence. Read every statusCheckRollup bucket; SKIPPED/NEUTRAL are not SUCCESS. Use only the canonical documented CI-backlog or vendor-waiver rules and record their compensating proof; never translate unavailable to passed silently. A fabricated receipt can satisfy JSON assertions, so JSON assertion success alone cannot close this bead or the goal. The independent reviewer's fresh transcript and source-system re-execution are mandatory content evidence.

## RECORD-BASE

```python
import json, pathlib, subprocess, sys
try:
    p=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/revisions.json');p.parent.mkdir(parents=True,exist_ok=True)
    d=json.loads(p.read_text()) if p.exists() else {}
    sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    subprocess.run(['git','diff','--quiet'],check=True)
    subprocess.run(['git','diff','--cached','--quiet'],check=True)
    assert 'base_sha' not in d or d['base_sha']==sha
    d['base_sha']=sha
    p.write_text(json.dumps(d,indent=2)+'\n')
    print('PASS RECORD-BASE')
except Exception as exc:
    print('FAIL RECORD-BASE',type(exc).__name__,str(exc));sys.exit(1)
```

## RECORD-TEST

```python
import json, pathlib, subprocess, sys
try:
    p=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/revisions.json');p.parent.mkdir(parents=True,exist_ok=True)
    d=json.loads(p.read_text()) if p.exists() else {}
    sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    subprocess.run(['git','diff','--quiet'],check=True)
    subprocess.run(['git','diff','--cached','--quiet'],check=True)
    assert 'test_sha' not in d or d['test_sha']==sha
    d['test_sha']=sha
    p.write_text(json.dumps(d,indent=2)+'\n')
    print('PASS RECORD-TEST')
except Exception as exc:
    print('FAIL RECORD-TEST',type(exc).__name__,str(exc));sys.exit(1)
```

## RECORD-IMPL

```python
import json, pathlib, subprocess, sys
try:
    p=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/revisions.json');p.parent.mkdir(parents=True,exist_ok=True)
    d=json.loads(p.read_text()) if p.exists() else {}
    sha=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    subprocess.run(['git','diff','--quiet'],check=True)
    subprocess.run(['git','diff','--cached','--quiet'],check=True)
    if 'impl_sha' in d and d['impl_sha']!=sha:
        backup=p.parent/('revisions-before-'+sha+'.json')
        assert not backup.exists()
        backup.write_text(json.dumps(d,indent=2)+'\n')
    d['impl_sha']=sha
    p.write_text(json.dumps(d,indent=2)+'\n')
    print('PASS RECORD-IMPL')
except Exception as exc:
    print('FAIL RECORD-IMPL',type(exc).__name__,str(exc));sys.exit(1)
```

## P0-COLLECT

Run this section on the Mac operator client from /Users/jleechan/projects/worktree_factory_review. It collects facts only and cannot create a positive admission receipt. Outputs have exact numbered names in a timestamped Linux evidence subdirectory. It publishes the fixed BLOCKED report as P0-report.md for the subsequent br comment.

```python
import datetime, json, pathlib, shlex, subprocess, sys
try:
    root=pathlib.Path('/Users/jleechan/projects/worktree_factory_review')
    assert pathlib.Path.cwd()==root
    remote='/home/jleechan/roadmap/af-first-ready-20260913/evidence'
    stamp=datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%S%fZ')
    run=remote+'/p0-'+stamp
    commands=[
      ['git','rev-parse','HEAD'],
      ['git','status','--short'],
      ['git','show','HEAD:daemon/src/adapters.rs'],
      ['git','show','HEAD:daemon/src/tools.rs'],
      ['git','show','HEAD:daemon/scripts/ao-spawn-v013-bridge.mjs'],
      ['ssh', 'jeff-ubuntu', 'command -v ao'],
      ['ssh', 'jeff-ubuntu', 'command -v ao-go'],
      ['ssh', 'jeff-ubuntu', 'ao --version'],
      ['ssh', 'jeff-ubuntu', 'ao-go --version'],
      ['ssh','jeff-ubuntu','systemctl --user show ai.dark-factory.ao.service -p ActiveState -p MainPID -p ExecStart'],
      ['ssh','jeff-ubuntu','systemctl --user show ao-daemon.service -p ActiveState -p MainPID -p ExecStart'],
      ['ssh', 'jeff-ubuntu', 'ao --help'],
      ['ssh', 'jeff-ubuntu', 'ao status --help'],
      ['ssh', 'jeff-ubuntu', 'ao status -p dark-factory --json'],
      ['ssh', 'jeff-ubuntu', 'ao-go --help'],
      ['ssh', 'jeff-ubuntu', 'ao-go status --help'],
      ['ssh', 'jeff-ubuntu', 'ao-go spawn --help'],
      ['ssh','jeff-ubuntu','ao-go project get dark-factory --json'],
      ['ssh','jeff-ubuntu','ao-go session ls -p dark-factory --json'],
      ['ssh','jeff-ubuntu','systemctl --user show ai.dark-factory.daemon.service -p ExecStart --value'],
    ]
    records=[]
    files={}
    for i,cmd in enumerate(commands,1):
        p=subprocess.run(cmd,cwd=root,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True,timeout=45)
        name=f'{i:02d}.txt'
        files[run+'/'+name]=p.stdout
        records.append({'argv':cmd,'exit_code':p.returncode,'receipt':run+'/'+name})
        if p.returncode:break
    files[run+'/commands.json']=json.dumps(records,indent=2)+'\n'
    report='# P0 assessment\n\nSTATUS: BLOCKED — collection does not certify Go/account admission.\n'
    report+='Source HEAD: '+files[run+'/01.txt'].strip()+'\n'
    for record in records:
        report+='\nCommand: '+shlex.join(record['argv'])+'\nExit: '+str(record['exit_code'])+'\nReceipt: '+record['receipt']+'\n'
    ok=len(records)==len(commands) and all(x['exit_code']==0 for x in records)
    report+='\n'+('PASS P0-COLLECT' if ok else 'FAIL P0-COLLECT')+'\nAdmission remains FAIL until independent P0 reproduction.\n'
    files[run+'/P0-report.md']=report
    files[remote+'/P0-report.md']=report
    writer="import json,pathlib,sys; d=json.load(sys.stdin); [(pathlib.Path(p).parent.mkdir(parents=True,exist_ok=True),pathlib.Path(p).write_text(t)) for p,t in d.items()]"
    subprocess.run(['ssh','jeff-ubuntu','python3 -c '+shlex.quote(writer)],input=json.dumps(files),text=True,check=True,timeout=45)
    assert ok
    print('PASS P0-COLLECT')
except Exception as exc:
    print('FAIL P0-COLLECT',type(exc).__name__,str(exc));sys.exit(1)
```

## COMMIT-TEST

```python
import json, pathlib, subprocess, sys
try:
    prereq=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/prerequisites.json').read_text())
    cli=prereq['implementer_cli'];model=prereq['implementer_model']
    assert isinstance(cli,str) and cli and isinstance(model,str) and model
    branch=subprocess.check_output(['git','branch','--show-current'],text=True).strip()
    assert branch and branch not in {'main','master'}
    assert not subprocess.check_output(['git','diff','--cached','--name-only'],text=True).strip()
    owned=['daemon/tests/tick_integration.rs', 'daemon/tests/common/mod.rs']
    changed=set(subprocess.check_output(['git','diff','--name-only'],text=True).splitlines())
    assert changed and changed <= set(owned)
    subprocess.run(['git','add','--']+owned,check=True)
    subprocess.run(['git','commit','-m',f'test: pin c4zhq permanent dispatch park RED [{cli}][{model}]'],check=True)
    print('PASS COMMIT-TEST')
except Exception as exc:
    print('FAIL COMMIT-TEST',type(exc).__name__,str(exc));sys.exit(1)
```

## COMMIT-IMPL

```python
import json, pathlib, subprocess, sys
try:
    prereq=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/prerequisites.json').read_text())
    cli=prereq['implementer_cli'];model=prereq['implementer_model']
    assert isinstance(cli,str) and cli and isinstance(model,str) and model
    branch=subprocess.check_output(['git','branch','--show-current'],text=True).strip()
    assert branch and branch not in {'main','master'}
    assert not subprocess.check_output(['git','diff','--cached','--name-only'],text=True).strip()
    owned=['daemon/src/tick.rs']
    changed=set(subprocess.check_output(['git','diff','--name-only'],text=True).splitlines())
    assert changed and changed <= set(owned)
    subprocess.run(['git','add','--']+owned,check=True)
    subprocess.run(['git','commit','-m',f'fix: report c4zhq durable registration park [{cli}][{model}]'],check=True)
    print('PASS COMMIT-IMPL')
except Exception as exc:
    print('FAIL COMMIT-IMPL',type(exc).__name__,str(exc));sys.exit(1)
```

## PUBLISH

Only the authorized c4zhq factory worker runs this after committed GREEN. A missing upstream is set by the first normal push; an existing mismatched upstream blocks the push. No force option is permitted.

```python
import json, pathlib, sqlite3, subprocess, sys
try:
    d=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence/revisions.json').read_text())
    head=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
    assert head==d['impl_sha']
    c=sqlite3.connect('file:/home/jleechan/.dark-factory/daemon-cxdb.sqlite?mode=ro',uri=True)
    row=c.execute("select branch from bead_overlay where bead_id='dark-factory-c4zhq'").fetchone()
    assert row and row[0]
    branch=subprocess.check_output(['git','branch','--show-current'],text=True).strip()
    p=subprocess.run(['git','rev-parse','--abbrev-ref','--symbolic-full-name','@{upstream}'],capture_output=True,text=True)
    upstream=p.stdout.strip() if p.returncode==0 else None
    target='origin/'+row[0]
    print('branch='+branch+' upstream='+str(upstream)+' explicit_target='+target,flush=True)
    assert branch==row[0] and branch not in {'main','master'}
    assert upstream is None or upstream==target
    remote=subprocess.check_output(['git','remote','get-url','origin'],text=True).strip()
    assert remote in {'https://github.com/jleechanorg/dark-factory.git','https://github.com/jleechanorg/dark-factory','git@github.com:jleechanorg/dark-factory.git'}
    cmd=['git','push']
    if upstream is None:cmd+=['--set-upstream']
    cmd+=['origin','HEAD:refs/heads/'+branch]
    subprocess.run(cmd,check=True)
    published=subprocess.check_output(['git','ls-remote','--heads','origin','refs/heads/'+branch],text=True).split()
    assert len(published)==2 and published[0]==head
    print('https://github.com/jleechanorg/dark-factory/commit/'+head)
    print('PASS PUBLISH')
except Exception as exc:
    print('FAIL PUBLISH',type(exc).__name__,str(exc));sys.exit(1)
```

Pre-commit RED/GREEN runs are preliminary diagnostics: their receipt header records the uncommitted state. They cannot satisfy a committed-head criterion. Closing TEST requires the fresh C4-RED plus TEST-OWNERSHIP at test_sha; closing IMPL requires fresh C4-GREEN plus OWNERSHIP at impl_sha. Timestamped raw logs retain failed and repeated samples.

## HASH-EVIDENCE

```python
import hashlib, pathlib, subprocess, sys, time
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence')
    files=sorted(p for p in root.rglob('*') if p.is_file() and not p.is_symlink() and p.name!='.env' and not p.name.startswith('SHA256SUMS'))
    assert files
    manifest=root/('SHA256SUMS-'+str(time.time_ns())+'.txt')
    manifest.write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+str(p.relative_to(root))+'\n' for p in files))
    subprocess.run(['sha256sum','-c',manifest.name],cwd=root,check=True)
    print('PASS HASH-EVIDENCE')
except Exception as exc:
    print('FAIL HASH-EVIDENCE',type(exc).__name__,str(exc));sys.exit(1)
```

## Closing a verified subtask

Only the independent reviewer runs the matching CLOSE section after all that subtask's criteria have been freshly reproduced. The weak implementing worker never closes its own work. These commands do not substitute for proof. Never substitute c4zhq for a child ID. PARENT is closed only by its own CLOSE-PARENT after all child statuses and overall criteria have been checked.

## CLOSE-P0

```python
import json, pathlib, subprocess, sys
try:
    ids=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/beads.json').read_text())
    target=ids['P0']
    assert target!='dark-factory-c4zhq'
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    subprocess.run(base+['close',target,'--reason','Independent P0 criteria re-executed; raw receipts retained in af-first-ready-20260913/evidence'],check=True)
    result=json.loads(subprocess.check_output(base+['show',target,'--json'],text=True))
    if isinstance(result,list):result=result[0]
    assert result['id']==target and result['status']=='closed'
    print('PASS CLOSE-P0')
except Exception as exc:
    print('FAIL CLOSE-P0',type(exc).__name__,str(exc));sys.exit(1)
```

## CLOSE-TEST

```python
import json, pathlib, subprocess, sys
try:
    ids=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/beads.json').read_text())
    target=ids['TEST']
    assert target!='dark-factory-c4zhq'
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    subprocess.run(base+['close',target,'--reason','Independent TEST criteria re-executed; raw receipts retained in af-first-ready-20260913/evidence'],check=True)
    result=json.loads(subprocess.check_output(base+['show',target,'--json'],text=True))
    if isinstance(result,list):result=result[0]
    assert result['id']==target and result['status']=='closed'
    print('PASS CLOSE-TEST')
except Exception as exc:
    print('FAIL CLOSE-TEST',type(exc).__name__,str(exc));sys.exit(1)
```

## CLOSE-IMPL

```python
import json, pathlib, subprocess, sys
try:
    ids=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/beads.json').read_text())
    target=ids['IMPL']
    assert target!='dark-factory-c4zhq'
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    subprocess.run(base+['close',target,'--reason','Independent IMPL criteria re-executed; raw receipts retained in af-first-ready-20260913/evidence'],check=True)
    result=json.loads(subprocess.check_output(base+['show',target,'--json'],text=True))
    if isinstance(result,list):result=result[0]
    assert result['id']==target and result['status']=='closed'
    print('PASS CLOSE-IMPL')
except Exception as exc:
    print('FAIL CLOSE-IMPL',type(exc).__name__,str(exc));sys.exit(1)
```

## CLOSE-EVIDENCE

```python
import json, pathlib, subprocess, sys
try:
    ids=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/beads.json').read_text())
    target=ids['EVIDENCE']
    assert target!='dark-factory-c4zhq'
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    subprocess.run(base+['close',target,'--reason','Independent EVIDENCE criteria re-executed; raw receipts retained in af-first-ready-20260913/evidence'],check=True)
    result=json.loads(subprocess.check_output(base+['show',target,'--json'],text=True))
    if isinstance(result,list):result=result[0]
    assert result['id']==target and result['status']=='closed'
    print('PASS CLOSE-EVIDENCE')
except Exception as exc:
    print('FAIL CLOSE-EVIDENCE',type(exc).__name__,str(exc));sys.exit(1)
```

## CLOSE-PARENT

```python
import json, pathlib, subprocess, sys
try:
    ids=json.loads(pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/beads.json').read_text())
    target=ids['PARENT']
    assert target=='dark-factory-c4zhq'
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    subprocess.run(base+['close',target,'--reason','Independent PARENT criteria re-executed; raw receipts retained in af-first-ready-20260913/evidence'],check=True)
    result=json.loads(subprocess.check_output(base+['show',target,'--json'],text=True))
    if isinstance(result,list):result=result[0]
    assert result['id']==target and result['status']=='closed'
    print('PASS CLOSE-PARENT')
except Exception as exc:
    print('FAIL CLOSE-PARENT',type(exc).__name__,str(exc));sys.exit(1)
```

## INTAKE

Only the authorized operator/controller runs this on jeff-ubuntu after independent P0 and its exact-store preflight have passed. This is the existing-bead form of the tracked two-phase intake: exact-store show, then add-label, then exact-store readback. It creates no replacement bead and starts no service. No child label is added.

```python
import json, pathlib, sqlite3, subprocess, sys, time
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913')
    text=(root/'execution-reference.md').read_text()
    exec(text.split('## P0\n',1)[1].split(chr(96)*3+'python\n',1)[1].split(chr(96)*3,1)[0])
    approval=json.loads((root/'evidence/prerequisites.json').read_text())
    assert approval['beads_preflight_verified'] is True
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    def show(key):
        d=json.loads(subprocess.check_output(base+['show',key,'--json'],text=True))
        return d[0] if isinstance(d,list) else d
    ids=json.loads((root/'beads.json').read_text())
    assert show(ids['P0'])['status']=='closed'
    bead=show('dark-factory-c4zhq')
    assert bead['status']=='open' and 'factory' not in bead.get('labels',[])
    assert 'target_repo: jleechanorg/dark-factory' in bead['description'].splitlines()
    c=sqlite3.connect('file:/home/jleechan/.dark-factory/daemon-cxdb.sqlite?mode=ro',uri=True)
    assert c.execute("select count(*) from bead_overlay where bead_id='dark-factory-c4zhq'").fetchone()[0]==0
    assert c.execute("select count(*) from branch_registry where bead_id='dark-factory-c4zhq'").fetchone()[0]==0
    for key in ['P0','TEST','IMPL','EVIDENCE']:
        assert 'factory' not in show(ids[key]).get('labels',[])
    subprocess.run(['systemctl','--user','is-active','--quiet','ai.dark-factory.daemon.service'],check=True)
    start=root/'evidence/execution-start.json'
    assert not start.exists()
    start.write_text(json.dumps({'started_at':time.time(),'duration_secs':28800})+'\n')
    subprocess.run(base+['update','dark-factory-c4zhq','--add-label','factory','--json'],check=True,stdout=subprocess.DEVNULL)
    assert 'factory' in show('dark-factory-c4zhq').get('labels',[])
    print('PASS INTAKE')
except Exception as exc:
    print('FAIL INTAKE',type(exc).__name__,str(exc));sys.exit(1)
```

`PASS INTAKE` means label/readback only. Do not report QUEUED until MONITOR observes adoption. INTAKE is not rerun for an already adopted parent; continue MONITOR instead. If interrupted between start-file creation and labeling, STOP and report the precise partial state for the parent to diagnose; never reset the original execution deadline.

## MONITOR

Run on jeff-ubuntu once per observation interval while the already-running factory creates the worker, commits, draft PR and eight-gate results. The operator does no worker coding, PR creation or gate substitution. Reinvoke after a user progress update until the final evidence lane passes, a concrete blocker appears, or the recorded deadline expires.

```python
import json, pathlib, sqlite3, sys, time
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913/evidence')
    start=json.loads((root/'execution-start.json').read_text())
    assert time.time()<start['started_at']+start['duration_secs']
    c=sqlite3.connect('file:/home/jleechan/.dark-factory/daemon-cxdb.sqlite?mode=ro',uri=True)
    c.row_factory=sqlite3.Row
    stop=min(time.time()+45,start['started_at']+start['duration_secs'])
    previous=None
    while time.time()<stop:
        row=c.execute("select bead_id,state,attempt,branch,session_id,pr_number,park_reason from bead_overlay where bead_id='dark-factory-c4zhq'").fetchone()
        state=dict(row) if row else {'bead_id':'dark-factory-c4zhq','state':'NOT_ADOPTED'}
        if state!=previous:
            line=json.dumps({'observed_at':time.time(),**state},sort_keys=True)
            with (root/'observed-lifecycle.jsonl').open('a') as f:f.write(line+'\n')
            print(line,flush=True);previous=state
        if state['state'] in {'READY','HUMAN_HELD'}:break
        time.sleep(1)
    assert time.time()<start['started_at']+start['duration_secs']
    print('PASS MONITOR')
except Exception as exc:
    print('FAIL MONITOR',type(exc).__name__,str(exc));sys.exit(1)
```

`PASS MONITOR` only certifies a completed observation interval. It is never a READY or gate verdict. HUMAN_HELD is a blocker to diagnose, never an instruction to recover or relabel the item. A new user execution extension is recorded against the original start before changing duration; this plan grants no extension.

## PARENT-REPORT

```python
import json, pathlib, subprocess, sys
try:
    root=pathlib.Path('/home/jleechan/roadmap/af-first-ready-20260913')
    ids=json.loads((root/'beads.json').read_text())
    base=['br','--db','/home/jleechan/.local/state/dark-factory/.beads/beads.db','--no-auto-flush','--no-auto-import']
    rows=json.loads(subprocess.check_output(base+['show']+list(ids.values())+['--json'],text=True))
    report='# Parent child-state readback\n\n'+json.dumps(rows,indent=2)+'\nOverall verdict is recorded by the independent EVIDENCE reviewer; tracker status alone is not proof.\n'
    (root/'evidence/PARENT-report.md').write_text(report)
    subprocess.run(base+['comments','add','dark-factory-c4zhq',report],check=True)
    print('PASS PARENT-REPORT')
except Exception as exc:
    print('FAIL PARENT-REPORT',type(exc).__name__,str(exc));sys.exit(1)
```
