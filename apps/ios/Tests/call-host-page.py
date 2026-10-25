"""Synthetic call page; no credentials, call service, or arbitrary file serving."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys

PAGE = b'''<!doctype html><body><script>
window.records=[];
window.PigeonpostNative={
 configure(o){records.push({kind:"configure",mailbox:o.mailbox.address,token:o.token})},
 signOut(){records.push({kind:"signOut"})},
 renewed(t){records.push({kind:"renewed",token:t})},
 answer(room,token){records.push({kind:"answer",room:room})},
 call(peer,kind){records.push({kind:"call",peer:peer,media:kind})},
 hangUp(){records.push({kind:"hangUp"})},
 async joinScheduled(code,o){records.push({...o,kind:"scheduled",media:o.kind,code:code});return window.joinResult || {state:"waiting"}}
};
window.webkit.messageHandlers.pigeonpost.postMessage({type:"ready"});
</script>'''

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        if self.path != '/native-call.html':
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/html; charset=utf-8')
        self.send_header('Cache-Control', 'no-store')
        self.send_header('Content-Length', str(len(PAGE)))
        self.end_headers()
        self.wfile.write(PAGE)

server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
Path(sys.argv[1]).write_text(str(server.server_port))
server.serve_forever()
