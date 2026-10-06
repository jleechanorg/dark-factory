"""Exercise the real scheduler against a controlled external GitHub boundary."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
JOB = ROOT / 'jobs/jleechanorg-pr-green/jleechanorg-pr-green-daily.sh'
SECRET = 'ghp_inspection_fixture_secret_1234567890'

GH = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
args=sys.argv[1:]; case=os.environ['INSPECTION_CASE']; root=Path(os.environ['FIXTURE'])
with (root/'gh-calls').open('a') as f:f.write(json.dumps(args)+'\n')
if args[:1]==['api']:
 if '/search/issues' in args:
  count=0 if case=='empty' else (2 if case=='mixed' else 1)
  items=[{'repository_url':'https://api.github.com/repos/jleechanorg/repo-a','number':n,'title':'fixture','html_url':f'https://github.com/jleechanorg/repo-a/pull/{n}','updated_at':'2099-01-01T00:00:00Z','draft':False} for n in range(1,count+1)]
  print(json.dumps([{'total_count':count,'incomplete_results':False,'items':items}]))
 elif '/protection' in args[-1]:print('{"required_status_checks":null}')
 else:print('[]')
 sys.exit(0)
if args[:2]!=['pr','view']:sys.exit(92)
number=args[2].rsplit('/',1)[-1]
counter=root/('attempts-'+number)
attempt=int(counter.read_text())+1 if counter.exists() else 1
counter.write_text(str(attempt))
if case in ('all_failed','privacy') or (case=='mixed' and number=='1'):
 print('GraphQL: Resource not accessible by integration (HTTP 403)',file=sys.stderr)
 if case=='privacy':
  print('Authorization: Bearer '+os.environ['GITHUB_TOKEN'],file=sys.stderr)
  print('token='+os.environ['GITHUB_TOKEN'],file=sys.stderr)
  print('https://user:password@example.invalid/?access_token=hidden',file=sys.stderr)
  print('x'*100000,file=sys.stderr)
 sys.exit(1)
if case=='transient_exhausted' or (case=='transient' and attempt==1):
 print('HTTP 503: Service Unavailable',file=sys.stderr);sys.exit(1)
if case=='unknown':
 print('Unclassified fixture failure',file=sys.stderr);sys.exit(17)
if case=='malformed':print('not JSON');sys.exit(0)
if case=='invalid_shape':print('{"headRefOid":"","statusCheckRollup":[]}');sys.exit(0)
print(json.dumps({'headRefOid':'a'*40,'baseRefName':'main','headRepository':{'nameWithOwner':'jleechanorg/repo-a'},'mergeable':'MERGEABLE','mergeStateStatus':'CLEAN','statusCheckRollup':None if case=='null_rollup' else []}))
'''


class InspectionRunTests(unittest.TestCase):
    def run_case(self, case):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        (root / 'bin').mkdir()
        (root / 'home').mkdir()
        (root / 'bin/gh').write_text(GH)
        (root / 'bin/ao').write_text('#!/bin/sh\necho unexpected AO call >&2\nexit 93\n')
        for path in (root / 'bin').iterdir():
            path.chmod(0o755)
        env = dict(os.environ, HOME=str(root / 'home'),
                   PATH=str(root / 'bin') + ':' + os.environ['PATH'],
                   PR_GREEN_METRICS_DIR=str(root / 'metrics'),
                   PR_GREEN_AO_SPAWN_LOCK_DIR=str(root / 'locks'),
                   PR_GREEN_RUN_STARTED='1791156600', PR_GREEN_DRY_RUN='1',
                   PR_GREEN_MAX_PRS='12', INSPECTION_CASE=case, FIXTURE=str(root),
                   GITHUB_TOKEN=SECRET, GH_TOKEN=SECRET)
        result = subprocess.run(['bash', str(JOB)], env=env, text=True,
                                capture_output=True, timeout=25)
        return root, result

    def metrics(self, root):
        path = root / 'metrics/runs.jsonl'
        self.assertTrue(path.is_file(), 'every completed scan must persist run metrics')
        return json.loads(path.read_text())

    def diagnostics(self, root, metrics):
        directory = Path(metrics['inspection_diagnostics_dir'])
        self.assertEqual(directory.stat().st_mode & 0o777, 0o700)
        self.assertTrue(directory.is_relative_to(root / 'metrics'))
        paths = list(directory.glob('*.json'))
        for path in paths:
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        return [(path, json.loads(path.read_text())) for path in paths]

    def test_all_failed_is_nonzero_with_failure_metrics_and_private_error(self):
        root, result = self.run_case('all_failed')
        self.assertNotEqual(result.returncode, 0, result.stdout)
        metrics = self.metrics(root)
        self.assertEqual((metrics['discovered'], metrics['analyzed'], metrics['analysis_failed']), (1, 0, 1))
        self.assertEqual(metrics['run_status'], 'failed')
        self.assertIn('analysis_failed=1', result.stdout)
        records = self.diagnostics(root, metrics)
        self.assertEqual(len(records), 1)
        record = records[0][1]
        self.assertEqual((record['repo'], record['number'], record['run_started']), ('repo-a', 1, 1791156600))
        self.assertEqual(record['attempts'][0]['classification'], 'authorization')
        self.assertIn('Resource not accessible', record['attempts'][0]['stderr'])
        self.assertEqual((root / 'attempts-1').read_text(), '1', 'authorization errors must not retry')

    def test_mixed_scan_counts_success_and_failure_without_losing_progress(self):
        root, result = self.run_case('mixed')
        self.assertEqual(result.returncode, 0, result.stderr)
        metrics = self.metrics(root)
        self.assertEqual((metrics['discovered'], metrics['analyzed'], metrics['analysis_failed']), (2, 1, 1))
        self.assertEqual(metrics.get('run_status'), 'partial')
        self.assertEqual(len(self.diagnostics(root, metrics)), 1)
        self.assertIn('run_status=partial', result.stdout)

    def test_valid_empty_org_is_successful_and_persists_zero_metrics(self):
        root, result = self.run_case('empty')
        self.assertEqual(result.returncode, 0, result.stderr)
        metrics = self.metrics(root)
        self.assertEqual((metrics['discovered'], metrics['analyzed'], metrics['analysis_failed']), (0, 0, 0))
        self.assertEqual(metrics['run_status'], 'empty')
        self.assertEqual(self.diagnostics(root, metrics), [])
        self.assertFalse((root / 'attempts-1').exists())

    def test_sensitive_and_oversized_errors_are_bounded_and_redacted(self):
        root, result = self.run_case('privacy')
        self.assertNotEqual(result.returncode, 0)
        records = self.diagnostics(root, self.metrics(root))
        self.assertEqual(len(records), 1)
        path, record = records[0]
        raw = path.read_text()
        self.assertNotIn(SECRET, raw + result.stdout + result.stderr)
        self.assertNotIn('user:password', raw)
        self.assertNotIn('access_token=hidden', raw)
        self.assertLess(path.stat().st_size, 20000)
        self.assertTrue(record['attempts'][0]['stderr_truncated'])

    def test_transient_read_failure_retries_once_and_records_recovery(self):
        root, result = self.run_case('transient')
        self.assertEqual(result.returncode, 0, result.stderr)
        metrics = self.metrics(root)
        self.assertEqual((metrics['analyzed'], metrics['analysis_failed'], metrics.get('run_status')), (1, 0, 'success'))
        record = self.diagnostics(root, metrics)[0][1]
        self.assertEqual(record['outcome'], 'recovered')
        self.assertEqual([a['classification'] for a in record['attempts']], ['transient_http', 'success'])

    def test_unknown_errors_are_not_retried(self):
        root, result = self.run_case('unknown')
        self.assertNotEqual(result.returncode, 0)
        record = self.diagnostics(root, self.metrics(root))[0][1]
        self.assertEqual(record['attempts'][0]['classification'], 'unknown')
        self.assertEqual((root / 'attempts-1').read_text(), '1')

    def test_malformed_success_body_is_failed_inspection(self):
        root, result = self.run_case('malformed')
        self.assertNotEqual(result.returncode, 0)
        record = self.diagnostics(root, self.metrics(root))[0][1]
        self.assertEqual(record['attempts'][0]['classification'], 'invalid_response')

    def test_missing_head_cannot_be_counted_as_analyzed(self):
        root, result = self.run_case('invalid_shape')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.metrics(root)['analysis_failed'], 1)

    def test_nullable_check_rollup_is_valid_empty_check_data(self):
        root, result = self.run_case('null_rollup')
        self.assertEqual(result.returncode, 0, result.stderr)
        metrics = self.metrics(root)
        self.assertEqual((metrics['analyzed'], metrics['analysis_failed']), (1, 0))

    def test_transient_retries_stop_after_second_failure(self):
        root, result = self.run_case('transient_exhausted')
        self.assertNotEqual(result.returncode, 0)
        record = self.diagnostics(root, self.metrics(root))[0][1]
        self.assertEqual(len(record['attempts']), 2)
        self.assertEqual((root / 'attempts-1').read_text(), '2')


if __name__ == '__main__':
    unittest.main()
