"""Synthetic call page; no credentials, call service, or arbitrary file serving."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import sys

PAGE = b'''<!doctype html><body><script>
window.records=[];
window.PigeonpostNative={
 configure(o){window.me=o.mailbox;records.push({kind:"configure",mailbox:o.mailbox.address,token:o.token})},
 signOut(){records.push({kind:"signOut"})},
 renewed(t){records.push({kind:"renewed",token:t})},
 answer(room,token){records.push({kind:"answer",room:room})},
 call(peer,kind){records.push({kind:"call",peer:peer,media:kind})},
 hangUp(){records.push({kind:"hangUp"})},
 async joinScheduled(code,o){records.push({...o,kind:"scheduled",media:o.kind,code:code});return window.joinResult || {state:"waiting"}},
 async watchStream(g,id){records.push({kind:"watch",group:g.id,name:g.name,stream:id});if(window.liveRefusal)return window.liveRefusal;window.liveNow={id:id,groupId:g.id,role:"viewer"};return {ok:true}},
 async goLive(g,o){records.push({kind:"goLive",group:g.id,title:o.title,camera:o.camera,screen:o.screen});window.liveNow={id:"str_"+"s".repeat(26),groupId:g.id,role:"host"};return {ok:true,stream:window.liveNow.id}},
 stopStream(){records.push({kind:"stopStream"});window.liveNow=null}
};
window.PigeonpostCallsHost={mailbox(){return window.me||null}};
window.PigeonpostLive={
 current(){return window.liveNow||null},
 show(){records.push({kind:"show"})},
 onGroupStream(e){records.push({kind:"groupStream",stream:e.stream_id,address:e.stage&&e.stage.address,state:e.stage&&e.stage.state})}
};
window.PigeonpostNotesKeys={
 async get(){return {key_id:"nkSYNTHETIC0000000000",public_key:"BPUB"}},
 label(){return "Desktop app"},
 async open(m){records.push({kind:"open",meeting:m.id,keys:(m.keys||[]).length});if(m.id==="mtg_nokey"){const e=new Error("x");e.code="no_key_for_device";throw e}return {title:"Synthetic",summary:"Done.",decisions:["Ship"]}}
};
window.confirmAnswer=null;
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
