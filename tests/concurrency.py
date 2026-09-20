"""Real worker concurrency regression: mock host counts overlapping network requests."""
import http.server
import json
import os
import pathlib
import struct
import subprocess
import sys
import tempfile
import threading
import time

ACCOUNT = {'id': 'fixture', 'name': 'Fixture', 'plan': 'plus', 'eligible': True,
           'binding': 'a' * 64, 'revision': 1, 'authenticationKind': 'oauth'}
STATE = 'gAAAAA' + 'x' * 286
lock = threading.Lock()
release = threading.Event()
release.set()
observed = {'active': 0, 'peak': 0, 'calls': 0, 'by_proxy': {}, 'peaks': {},
            'delay': .25, 'stagger': False, 'gets': 0, 'match': True}


class Host(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        assert self.headers['Authorization'] == 'Bearer fixture'
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method, data = request['method'], request['input']
        if method == 'accounts.list':
            result = [ACCOUNT]
        elif method == 'accounts.get':
            with lock:
                observed['gets'] += 1
                delayed = observed['stagger'] and observed['gets'] in (2, 3)
            if delayed:
                time.sleep(.35)
            result = ACCOUNT
        elif method == 'responses.probe':
            assert data['credentialScope'] == ACCOUNT['binding']
            proxy = data['proxy']
            with lock:
                observed['active'] += 1
                observed['calls'] += 1
                observed['peak'] = max(observed['peak'], observed['active'])
                observed['by_proxy'][proxy] = observed['by_proxy'].get(proxy, 0) + 1
                observed['peaks'][proxy] = max(observed['peaks'].get(proxy, 0), observed['by_proxy'][proxy])
                delay = observed['delay']
            assert release.wait(8), 'test did not release probe'
            time.sleep(delay)
            with lock:
                observed['active'] -= 1
                observed['by_proxy'][proxy] -= 1
            result = [200, STATE if observed['match'] else '', 0, 'fixture']
        else:
            raise AssertionError(method)
        body = json.dumps(result).encode()
        self.send_response(200)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


http.server.ThreadingHTTPServer.request_queue_size = 128
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Host)
threading.Thread(target=server.serve_forever, daemon=True).start()


def wait_for(condition, message):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        if condition():
            return
        time.sleep(.025)
    raise AssertionError(message)


def reset_stats(**options):
    with lock:
        assert observed['active'] == 0
        observed.update(active=0, peak=0, calls=0, by_proxy={}, peaks={},
                        delay=.25, stagger=False, gets=0, match=True)
        observed.update(options)


with tempfile.TemporaryDirectory() as root:
    proc = subprocess.Popen([sys.argv[1]], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, env={**os.environ,
                            'RS_PLUGIN_DATA_DIR': root,
                            'RS_PLUGIN_SERVICES_URL': f'http://127.0.0.1:{server.server_port}/invoke',
                            'RS_PLUGIN_SERVICES_TOKEN': 'Bearer fixture'})
    serial = 0

    def rpc(method, params=None):
        global serial
        serial += 1
        body = json.dumps({'jsonrpc': '2.0', 'id': serial, 'method': method,
                           'params': params or {}}).encode()
        proc.stdin.write(struct.pack('>I', len(body)) + body)
        proc.stdin.flush()
        prefix = proc.stdout.read(4)
        assert len(prefix) == 4, 'worker closed'
        result = json.loads(proc.stdout.read(struct.unpack('>I', prefix)[0]))
        assert 'error' not in result, result
        return result['result']

    def finish(job):
        result = {}

        def done():
            nonlocal result
            result = rpc('admin.job', {'id': job['jobId']})
            return not result.get('pending')

        wait_for(done, 'job timeout')
        return result

    def configure(proxies, mode='manual', **changes):
        panel = rpc('admin.panel')
        settings = panel['settings']
        settings.update(enabled=True, manualIntervalSeconds=0, models=['gpt-6-astra'],
                        accounts={'fixture': {'mode': mode, 'targetLength': None}},
                        intervalSeconds=10, activityOnly=False, **changes)
        return rpc('admin.update', {'revision': panel['revision'], 'settings': settings,
                                    'proxies': proxies})

    def proxy(port, concurrency=3, **extra):
        return {'name': f'Proxy {port}', 'url': f'http://127.0.0.1:{port}',
                'enabled': True, 'concurrency': concurrency, **extra}

    probe = {'accountId': 'fixture', 'model': 'gpt-6-astra'}
    try:
        rpc('initialize', {'apiVersion': 1, 'pluginId': 'turn-state'})
        panel = configure([proxy(8000)])
        release.clear()
        job = rpc('admin.probe', probe)
        wait_for(lambda: observed['active'] == 3, 'manual batch never reached 3 concurrent requests')
        status = rpc('admin.panel')
        assert status['proxies'][0]['inFlight'] == 3
        duplicate = finish(rpc('admin.probe', probe))
        assert 'error' in duplicate, duplicate
        assert observed['calls'] == 3, observed
        release.set()
        assert finish(job)['result']['matched']
        assert observed['peak'] == 3
        assert len(rpc('admin.panel')['logs']) == 3
        print('PASS manual pool concurrency=3, live counters, duplicate batch rejection')

        # Every alias for the same network endpoint shares the minimum configured limit.
        panel = configure([proxy(8000), proxy(8000, 1, name='Alias'), proxy(8001, enabled=False)])
        reset_stats()
        assert finish(rpc('admin.probe', probe))['result']['matched']
        assert observed['calls'] == observed['peak'] == 1, observed
        print('PASS duplicate endpoint limit and disabled proxy exclusion')

        panel = configure([proxy(8000), proxy(8001)])
        reset_stats()
        assert finish(rpc('admin.probe', {**probe, 'proxyId': '1', 'revision': panel['revision']}))['result']['matched']
        assert observed['peak'] == observed['calls'] == 3
        assert list(observed['peaks']) == ['http://127.0.0.1:8001']
        print('PASS selected proxy concurrency=3')

        # Global ceiling still applies even with five independently configured entries.
        configure([proxy(8000 + index) for index in range(5)])
        reset_stats()
        assert finish(rpc('admin.probe', probe))['result']['matched']
        assert observed['calls'] == observed['peak'] == 12, observed
        assert max(observed['peaks'].values()) == 3
        print('PASS global ceiling=12 and per-entry ceiling=3')

        configure([proxy(8000)])
        ACCOUNT['binding'] = 'b' * 64
        reset_stats()
        rpc('admin.continuous', {**probe, 'intervalSeconds': 10})
        wait_for(lambda: rpc('admin.panel')['accounts'][0]['models'][0]['continuous'] is None,
                 'continuous did not stop after hit')
        assert observed['peak'] == observed['calls'] == 3, observed
        print('PASS continuous concurrency=3 and automatic stop after hit')

        ACCOUNT['binding'] = 'd' * 64
        reset_stats()
        release.clear()
        rpc('admin.continuous', {**probe, 'intervalSeconds': 10})
        wait_for(lambda: observed['active'] == 3, 'continuous stop fixture did not start')
        panel = rpc('admin.continuous', {**probe, 'intervalSeconds': None})
        assert panel['accounts'][0]['models'][0]['continuous'] is None
        assert panel['accounts'][0]['models'][0]['busy']
        assert 'error' in finish(rpc('admin.probe', probe)), 'stop released slots before real requests drained'
        release.set()
        wait_for(lambda: not rpc('admin.panel')['accounts'][0]['models'][0]['busy'], 'stopped batch did not drain')
        assert observed['calls'] == observed['peak'] == 3
        print('PASS explicit stop drains in-flight requests without releasing quotas early')

        reset_stats(delay=0, stagger=True)
        assert finish(rpc('admin.probe', probe))['result']['matched']
        assert observed['calls'] == 1, observed
        print('PASS first hit prevents queued slots from starting new probes')

        # The old automatic path checked cooldown inside each slot: first fast completion
        # suppressed the two slots whose account snapshots arrived slightly later.
        ACCOUNT['binding'] = 'c' * 64
        reset_stats(delay=0, stagger=True, match=False)
        configure([proxy(8000)], mode='auto')
        wait_for(lambda: observed['calls'] >= 3, 'fast first response suppressed sibling slots')
        wait_for(lambda: not rpc('admin.panel')['accounts'][0]['models'][0]['busy'], 'batch still busy')
        assert observed['calls'] == 3, observed
        print('PASS automatic batch preserves all 3 slots despite fast first response')

        configure([proxy(8000)], mode='manual')
        release.clear()
        reset_stats()
        job = rpc('admin.probe', probe)
        wait_for(lambda: observed['active'] == 3, 'batch not started')
        panel = rpc('admin.panel')
        panel['settings']['plusProLength'] = 300
        rpc('admin.update', {'revision': panel['revision'], 'settings': panel['settings']})
        release.set()
        assert not finish(job)['result']['matched'], 'stale results adopted after policy edit'
        assert pathlib.Path(root, 'turn-state-tickets.json').exists()
        print('PASS policy change rejects all stale in-flight results')
    finally:
        release.set()
        proc.kill()
        proc.wait()
        server.shutdown()
