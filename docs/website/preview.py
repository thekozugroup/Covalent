#!/usr/bin/env python3
"""Serve the unchanged WebUI with synthetic, read-only screenshot data.

Run from a repository checkout: python3 docs/website/preview.py [--receiver]
No files transfer, no credentials load, and no production endpoints are used.
"""
import argparse
import json
import mimetypes
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from time import time
from urllib.parse import urlsplit

WEB = Path(__file__).resolve().parents[2] / 'packaging' / 'web'
SOURCE = '11111111-1111-4111-8111-111111111111'
HOME = '22222222-2222-4222-8222-222222222222'
ARCHIVE = '33333333-3333-4333-8333-333333333333'
LAPTOP = '44444444-4444-4444-8444-444444444444'
FOLDER = '55555555-5555-4555-8555-555555555555'
NAMES = {SOURCE: 'Studio', HOME: 'Home NAS', ARCHIVE: 'Archive NAS', LAPTOP: 'Laptop'}


def example(receiver=False):
    now = int(time() * 1000)
    policy = dict(propagateSourceDeletions=False, restoreLocalDeletions=False)
    settings = dict(revision=1, settings=dict(deletionPolicy=policy, paused=False,
        cadence=dict(mode='scheduled', intervalMinutes=60),
        androidConditions=dict(wifiOnly=False, chargingOnly=False)),
        changeId='66666666-6666-4666-8666-666666666666', changedBy=SOURCE,
        confirmed=True, pendingChange=None, conflictedChange=None)
    run = dict(generation=8, stateRevision=2, settingsRevision=1, phase='succeeded',
        startedAtUnixMs=now-600000, deadlineUnixMs=now+85800000,
        endedAtUnixMs=now-540000, nextDueAtUnixMs=now+3000000,
        pendingRequest=None, rejectedRequest=None,
        destinations=[dict(peerId=p, result='succeeded', endedAtUnixMs=now-540000)
                      for p in (HOME, ARCHIVE)])
    endpoint = lambda p: dict(deviceId=p, displayName=NAMES[p])
    roster = dict(revision=2, label='Music', source=endpoint(SOURCE),
                  destinations=[endpoint(HOME), endpoint(ARCHIVE)])
    shares = [dict(offerId=f'{n}7777777-7777-4777-8777-777777777777', folderId=FOLDER,
        label='Music', peerId=SOURCE if receiver else peer, incoming=receiver,
        phase='ready', expiresAtUnixMs=None, expired=False, peerConnection='connected',
        linkPolicy=policy, linkSettings=settings, linkRun=run, endpointRoster=roster)
        for n, peer in enumerate((HOME,) if receiver else (HOME, ARCHIVE), 1)]
    peers = (SOURCE,) if receiver else (HOME, ARCHIVE, LAPTOP)
    return dict(schemaVersion=1, availability='available', lifecycle='stopped', issue=None,
        healthFreshness='fresh', connectionFreshness='fresh',
        peers=[dict(peerId=p, displayName=NAMES[p], address=f'192.0.2.{i+10}:8787')
               for i, p in enumerate(peers)], shares=shares, folders=[])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receiver', action='store_true')
    parser.add_argument('--port', type=int, default=0)
    args = parser.parse_args()
    state = example(args.receiver)
    local = HOME if args.receiver else SOURCE

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def respond(self, value, code=200):
            self.send_bytes(json.dumps(value).encode(), 'application/json', code)

        def send_bytes(self, body, content_type, code=200):
            self.send_response(code)
            self.send_header('Content-Type', content_type)
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            path = urlsplit(self.path).path
            routes = {
                '/api/v1/status': dict(protocolVersion=1, deviceName=NAMES[local], state='ready', lanDiscovery=True),
                '/api/v1/transport/identity': dict(deviceId=local),
                '/api/v1/sync/status': state,
                '/api/v1/pair/network/pending': [],
                '/api/v1/discovery': dict(candidates=[], lan='available', tailscale='available'),
            }
            if path in routes:
                return self.respond(routes[path])
            name = 'index.html' if path == '/' else path.removeprefix('/assets/')
            if '/' in name or not (WEB / name).is_file():
                return self.respond({'code': 'not_found'}, 404)
            self.send_bytes((WEB / name).read_bytes(), mimetypes.guess_type(name)[0] or 'application/octet-stream')

        def do_POST(self):
            if self.path == '/api/v1/config/export':
                return self.respond({})
            self.respond({'code': 'example_read_only', 'message': 'This screenshot preview cannot change files or settings.'}, 405)

    server = ThreadingHTTPServer(('127.0.0.1', args.port), Handler)
    print(f'Example data only: http://127.0.0.1:{server.server_port}/', flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == '__main__':
    main()
