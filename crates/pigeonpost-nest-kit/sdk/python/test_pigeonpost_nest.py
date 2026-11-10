import unittest
import os
import sys
import json
import tempfile
import threading
import http.server
import shutil

# Insert current directory into sys.path to allow importing pigeonpost_nest
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import pigeonpost_nest as sdk


class FakeBrokerHandler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        self._handle_request('GET')

    def do_POST(self):
        self._handle_request('POST')

    def _handle_request(self, method):
        content_length = int(self.headers.get('Content-Length', 0))
        body_bytes = self.rfile.read(content_length) if content_length > 0 else b''
        
        try:
            body = json.loads(body_bytes.decode('utf-8')) if body_bytes else {}
        except ValueError:
            body = {}

        headers_dict = {k.lower(): v for k, v in self.headers.items()}
        
        server = self.server
        request_info = {
            'method': method,
            'path': self.path,
            'headers': headers_dict,
            'body': body
        }
        server.requests.append(request_info)

        status, response_body = server.responder(method, self.path, body)
        
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        resp_bytes = json.dumps(response_body).encode('utf-8')
        self.send_header('Content-Length', str(len(resp_bytes)))
        self.end_headers()
        self.wfile.write(resp_bytes)


class TestPigeonpostNest(unittest.TestCase):
    def setUp(self):
        self.root_dir = tempfile.mkdtemp()
        os.makedirs(os.path.join(self.root_dir, 'jobs', 'in'), exist_ok=True)
        os.makedirs(os.path.join(self.root_dir, 'jobs', 'out'), exist_ok=True)
        os.makedirs(os.path.join(self.root_dir, 'run'), exist_ok=True)
        
        # Create broker token
        token_path = os.path.join(self.root_dir, 'run', 'broker-token')
        with open(token_path, 'w') as f:
            f.write('nbt_' + '0' * 40)

        self.server = None
        self.thread = None
        self.requests = []
        
    def _start_server(self, responder_func=None):
        class Handler(FakeBrokerHandler):
            pass
        
        # We need to attach requests and responder to the server instance
        # But BaseHTTPRequestHandler accesses self.server which is the ThreadingHTTPServer instance.
        # So we add attributes to the server object.
        
        def default_responder(method, path, body):
            return (200, {})

        self.responder = responder_func or default_responder
        
        self.server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        self.server.requests = []
        self.server.responder = self.responder
        
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        
    def tearDown(self):
        if self.server:
            self.server.shutdown()
            self.server.server_close()
        if os.path.exists(self.root_dir):
            shutil.rmtree(self.root_dir)

    def _write_job(self, job_id, operation, version, payload=None, config=None):
        path = os.path.join(self.root_dir, 'jobs', 'in', f'{job_id}.json')
        data = {
            'job_id': job_id,
            'operation': operation,
            'operation_version': version,
            'payload': payload or {}
        }
        if config is not None:
            data['config'] = config
        with open(path, 'w') as f:
            json.dump(data, f)
        return path

    def _read_result(self, job_id):
        path = os.path.join(self.root_dir, 'jobs', 'out', f'{job_id}.json')
        if not os.path.exists(path):
            return None
        with open(path, 'r') as f:
            return json.load(f)

    def test_secrets_come_from_the_broker_only(self):
        def responder(method, path, body):
            if path == '/v1/secrets/api_token':
                return (200, {'name': 'api_token', 'value': 'synthetic-secret'})
            return (404, {'error': 'secret_not_set'})

        self._start_server(responder)
        url = f'http://127.0.0.1:{self.server.server_address[1]}'
        broker = sdk.Broker(url=url, root=self.root_dir)
        self.assertEqual(broker.secret('api_token'), 'synthetic-secret')
        req = self.server.requests[-1]
        self.assertEqual(req['method'], 'GET')
        self.assertEqual(req['headers']['authorization'], 'Bearer nbt_' + '0' * 40)
        with self.assertRaises(sdk.BrokerError) as e:
            broker.secret('missing')
        self.assertEqual(e.exception.code, 'secret_not_set')
        with self.assertRaises(sdk.NestError) as e:
            broker.secret('../budget')
        self.assertEqual(e.exception.code, 'invalid_secret_name')
        self.assertEqual(len(self.server.requests), 2, 'a bad name is never sent')

    def test_1_echo_success(self):
        self._start_server()
        
        def handler(job):
            return {'answer': {'x': 1}, 'sources': ['a']}

        handlers = {'demo.echo@1.0': handler}
        job_id = 'job_aaaaaaaaaaaaaaaaaaaaaaaaaa'
        self._write_job(job_id, 'demo.echo', '1.0')
        
        count = sdk.run_once(handlers, root=self.root_dir, url='http://127.0.0.1:1') # Port doesn't matter for this test if no broker call
        
        self.assertEqual(count, 1)
        
        result = self._read_result(job_id)
        self.assertIsNotNone(result)
        self.assertEqual(result['job_id'], job_id)
        self.assertEqual(result['status'], 'succeeded')
        self.assertEqual(result['answer'], {'x': 1})
        self.assertEqual(result['sources'], ['a'])
        self.assertFalse(result['model_used'])
        
        inbox_path = os.path.join(self.root_dir, 'jobs', 'in', f'{job_id}.json')
        self.assertFalse(os.path.exists(inbox_path))

    def test_2_unknown_operation(self):
        self._start_server()
        handlers = {}
        job_id = 'job_unknown'
        self._write_job(job_id, 'demo.unknown', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url='http://127.0.0.1:1')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['error_code'], 'unknown_operation')

    def test_3_invalid_json(self):
        self._start_server()
        handlers = {}
        job_id = 'job_invalid'
        path = os.path.join(self.root_dir, 'jobs', 'in', f'{job_id}.json')
        with open(path, 'w') as f:
            f.write('{invalid json}')
            
        sdk.run_once(handlers, root=self.root_dir, url='http://127.0.0.1:1')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['error_code'], 'invalid_job')

    def test_4_handler_errors(self):
        self._start_server()
        
        # Case A: NestError
        job_id_a = 'job_err_nest'
        def handler_nest(job):
            raise sdk.NestError('not_found')
            
        handlers = {'test.nest@1.0': handler_nest}
        self._write_job(job_id_a, 'test.nest', '1.0')
        
        # Case B: RuntimeError
        job_id_b = 'job_err_rt'
        def handler_rt(job):
            raise RuntimeError('boom')
            
        handlers['test.rt@1.0'] = handler_rt
        self._write_job(job_id_b, 'test.rt', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url='http://127.0.0.1:1')
        
        res_a = self._read_result(job_id_a)
        self.assertEqual(res_a['status'], 'failed')
        self.assertEqual(res_a['error_code'], 'not_found')
        
        res_b = self._read_result(job_id_b)
        self.assertEqual(res_b['status'], 'failed')
        self.assertEqual(res_b['error_code'], 'internal_error')

    def test_5_broker_complete_success(self):
        def responder(method, path, body):
            if method == 'POST' and path == '/v1/chat/completions':
                return (200, {
                    'choices': [{'message': {'content': 'hi'}}],
                    'usage': {'prompt_tokens': 7, 'completion_tokens': 2}
                })
            return (404, {})

        self._start_server(responder)
        
        job_id = 'job_broker'
        def handler(job):
            res = job.broker.complete(system='s', user='u', max_tokens=50)
            # Return just the text to ensure model_used is detected by SDK
            return res['text']
            
        handlers = {'test.broker@1.0': handler}
        self._write_job(job_id, 'test.broker', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'succeeded')
        self.assertTrue(result['model_used'])
        self.assertEqual(result['answer'], 'hi')
        
        # Check request details
        reqs = self.server.requests
        self.assertEqual(len(reqs), 1)
        req = reqs[0]
        self.assertEqual(req['method'], 'POST')
        self.assertEqual(req['path'], '/v1/chat/completions')
        self.assertIn('authorization', req['headers'])
        # Token check is implicit via server setup, but we can check header format loosely or exact if token known.
        # The SDK sends Bearer <token>. We know the token content from setUp.
        expected_auth = 'Bearer nbt_' + '0' * 40
        self.assertEqual(req['headers']['authorization'], expected_auth)
        
        body = req['body']
        self.assertIn('messages', body)
        self.assertEqual(len(body['messages']), 2)
        self.assertEqual(body['messages'][0], {'role': 'system', 'content': 's'})
        self.assertEqual(body['messages'][1], {'role': 'user', 'content': 'u'})
        self.assertEqual(body['max_tokens'], 50)

    def test_6_broker_retry(self):
        call_count = [0]
        def responder(method, path, body):
            if method == 'POST' and path == '/v1/chat/completions':
                call_count[0] += 1
                if call_count[0] == 1:
                    return (409, {'error': 'busy'})
                else:
                    return (200, {
                        'choices': [{'message': {'content': 'hi'}}],
                        'usage': {}
                    })
            return (404, {})

        self._start_server(responder)
        
        job_id = 'job_retry'
        def handler(job):
            res = job.broker.complete(user='u')
            return res['text']
            
        handlers = {'test.retry@1.0': handler}
        self._write_job(job_id, 'test.retry', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'succeeded')
        self.assertEqual(call_count[0], 2)

    def test_7_broker_allowance_exhausted(self):
        def responder(method, path, body):
            if method == 'POST' and path == '/v1/chat/completions':
                return (429, {'error': 'allowance_exhausted'})
            return (404, {})

        self._start_server(responder)
        
        job_id = 'job_allowance'
        def handler(job):
            # Let error propagate
            res = job.broker.complete(user='u')
            return res['text']
            
        handlers = {'test.allow@1.0': handler}
        self._write_job(job_id, 'test.allow', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['error_code'], 'allowance_exhausted')
        self.assertEqual(len(self.server.requests), 1)

    def test_8_effect_approval_required(self):
        def responder(method, path, body):
            if method == 'POST' and path == '/v1/effects':
                return (202, {'state': 'awaiting_approval'})
            return (404, {})

        self._start_server(responder)
        
        job_id = 'job_eff_appr'
        def handler(job):
            try:
                job.effect(key='k1', kind='ticket.create', body={'a': 1}, requires_approval=True)
                return 'done'
            except sdk.NestError as e:
                raise e # Propagate to test error code

        handlers = {'test.eff@1.0': handler}
        self._write_job(job_id, 'test.eff', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'failed')
        self.assertEqual(result['error_code'], 'approval_required')
        
        reqs = self.server.requests
        self.assertEqual(len(reqs), 1)
        req = reqs[0]
        self.assertEqual(req['path'], '/v1/effects')
        self.assertEqual(req['headers']['idempotency-key'], 'k1')
        self.assertTrue(req['body']['requires_approval'])

    def test_9_effect_done_idempotent(self):
        call_count = [0]
        def responder(method, path, body):
            if method == 'POST' and path == '/v1/effects':
                call_count[0] += 1
                return (200, {'state': 'done', 'outcome': {'ref': 'r1'}})
            return (404, {})

        self._start_server(responder)
        
        job_id = 'job_eff_done'
        def handler(job):
            r1 = job.effect(key='k1', kind='ticket.create')
            r2 = job.effect(key='k1', kind='ticket.create') # Should not make request
            return {'r1': r1, 'r2': r2}

        handlers = {'test.eff2@1.0': handler}
        self._write_job(job_id, 'test.eff2', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'succeeded')
        self.assertEqual(result['answer']['r1'], {'ref': 'r1'})
        self.assertEqual(result['answer']['r2'], {'ref': 'r1'})
        self.assertEqual(call_count[0], 1)

    def test_10_effect_errors(self):
        # Case A: 404
        def responder_404(method, path, body):
            if method == 'POST' and path == '/v1/effects':
                return (404, {})
            return (200, {})

        self._start_server(responder_404)
        
        job_id_404 = 'job_eff_404'
        def handler_404(job):
            job.effect(key='k1', kind='ticket.create')
            return 'ok'

        handlers = {'test.eff3@1.0': handler_404}
        self._write_job(job_id_404, 'test.eff3', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        res = self._read_result(job_id_404)
        self.assertEqual(res['status'], 'failed')
        self.assertEqual(res['error_code'], 'effects_unavailable')

        # Case B: Invalid Key (no request)
        self.tearDown()
        self.setUp()
        
        def responder_no_req(method, path, body):
            return (200, {})
            
        self._start_server(responder_no_req)
        
        job_id_bad = 'job_eff_bad'
        def handler_bad(job):
            job.effect(key='bad key', kind='ticket.create')
            return 'ok'

        handlers = {'test.eff4@1.0': handler_bad}
        self._write_job(job_id_bad, 'test.eff4', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        res_bad = self._read_result(job_id_bad)
        self.assertEqual(res_bad['status'], 'failed')
        self.assertEqual(res_bad['error_code'], 'invalid_effect')
        self.assertEqual(len(self.server.requests), 0)

    def test_11_progress_failure_ignored(self):
        def responder(method, path, body):
            if method == 'POST' and path == '/v1/progress':
                return (500, {})
            return (200, {})

        self._start_server(responder)
        
        job_id = 'job_prog'
        def handler(job):
            job.progress(0.5, 'half')
            return 'ok'

        handlers = {'test.prog@1.0': handler}
        self._write_job(job_id, 'test.prog', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url=f'http://127.0.0.1:{self.server.server_address[1]}')
        
        result = self._read_result(job_id)
        self.assertEqual(result['status'], 'succeeded')
        
        reqs = [r for r in self.server.requests if r['path'] == '/v1/progress']
        self.assertEqual(len(reqs), 1)
        body = reqs[0]['body']
        self.assertEqual(body['job_id'], job_id)
        self.assertEqual(body['fraction'], 0.5)
        self.assertEqual(body['message'], 'half')

    def test_12_config(self):
        self._start_server()
        
        # Case A: the job carries a configuration
        job_id_conf = 'job_conf'
        def handler_conf(job):
            return {'tone': job.config['tone']}

        handlers = {'test.conf@1.0': handler_conf}
        self._write_job(job_id_conf, 'test.conf', '1.0', config={'tone': 'short'})
        
        sdk.run_once(handlers, root=self.root_dir, url='http://127.0.0.1:1')
        
        res = self._read_result(job_id_conf)
        self.assertEqual(res['answer']['tone'], 'short')

        # Case B: no configuration
        # Remove result to ensure fresh run or use new job id
        job_id_noconf = 'job_noconf'
        def handler_noconf(job):
            return {'config_empty': dict(job.config) == {}}

        handlers['test.noconf@1.0'] = handler_noconf
        self._write_job(job_id_noconf, 'test.noconf', '1.0')
        
        sdk.run_once(handlers, root=self.root_dir, url='http://127.0.0.1:1')
        
        res2 = self._read_result(job_id_noconf)
        self.assertTrue(res2['answer']['config_empty'])

    def test_13_result_of(self):
        # Directly test sdk.result_of
        
        # Success case
        r = sdk.result_of('j', 'plain', False)
        self.assertEqual(r['job_id'], 'j')
        self.assertEqual(r['status'], 'succeeded')
        self.assertEqual(r['answer'], 'plain')
        self.assertEqual(r['sources'], [])
        self.assertFalse(r['model_used'])

        # Too large case
        huge_answer = 'a' * (600 * 1024)
        r_large = sdk.result_of('j', huge_answer, False)
        self.assertEqual(r_large['status'], 'failed')
        self.assertEqual(r_large['error_code'], 'result_too_large')

if __name__ == '__main__':
    unittest.main()