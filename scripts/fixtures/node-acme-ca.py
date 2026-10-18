#!/usr/bin/env python3
"""Local-only DNS and HTTPS directory proxy for disposable node acceptance."""
import http.server
import json
from pathlib import Path
import socketserver
import ssl
import struct
import threading
import time
import urllib.error
import urllib.request

ROOT = Path('/var/tmp/acme-fixtures')

def dns_answer(request):
    offset = 12
    while request[offset]: offset += request[offset] + 1
    offset += 1
    kind, _ = struct.unpack('!HH', request[offset:offset+4])
    question = request[12:offset+4]
    answer = b'\xc0\x0c\x00\x01\x00\x01\x00\x00\x00\x01\x00\x04\x7f\x00\x00\x01' if kind == 1 else b''
    return request[:2] + struct.pack('!HHHHH', 0x8400, 1, int(bool(answer)), 0, 0) + question + answer

class UDP(socketserver.BaseRequestHandler):
    def handle(self): self.request[1].sendto(dns_answer(self.request[0]), self.client_address)

class TCP(socketserver.StreamRequestHandler):
    def handle(self):
        length = self.rfile.read(2)
        if length:
            answer = dns_answer(self.rfile.read(struct.unpack('!H', length)[0]))
            self.wfile.write(struct.pack('!H', len(answer)) + answer)

class Proxy(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_GET(self): self.forward()
    def do_HEAD(self): self.forward()
    def do_POST(self): self.forward()
    def forward(self):
        path = '/dir' if self.path == '/directory' else self.path
        data = self.rfile.read(int(self.headers.get('Content-Length', '0'))) if self.command == 'POST' else None
        request = urllib.request.Request('https://127.0.0.1:14000'+path, data=data, method=self.command)
        request.add_header('Host',self.headers['Host'])
        for key, value in self.headers.items():
            if key.lower() not in ('host','connection','content-length'): request.add_header(key,value)
        try:
            result = urllib.request.urlopen(request, context=ssl.create_default_context(cafile=str(ROOT/'ca.crt')), timeout=30)
        except urllib.error.HTTPError as error: result = error
        with result:
            body = result.read()
            waiting = self.path.startswith('/authZ/') and (ROOT/'delay-ca').exists()
            if waiting and result.status == 200:
                status = json.loads(body)
                status['status'] = 'pending'
                body = json.dumps(status).encode()
            self.send_response(result.status)
            for key,value in result.headers.items():
                if key.lower() not in ('connection','transfer-encoding','content-length'): self.send_header(key,value)
            if waiting: self.send_header('Retry-After','1')
            self.send_header('Content-Length',str(len(body))); self.end_headers()
            if self.command != 'HEAD':
                try: self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError): pass

if __name__ == '__main__':
    udp=socketserver.ThreadingUDPServer(('127.0.0.1',1053),UDP)
    tcp=socketserver.ThreadingTCPServer(('127.0.0.1',1053),TCP)
    for server in (udp,tcp): threading.Thread(target=server.serve_forever,daemon=True).start()
    proxy=http.server.ThreadingHTTPServer(('127.0.0.1',443),Proxy)
    context=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(str(ROOT/'endpoint.crt'),str(ROOT/'endpoint.key'))
    proxy.socket=context.wrap_socket(proxy.socket,server_side=True)
    proxy.serve_forever()
