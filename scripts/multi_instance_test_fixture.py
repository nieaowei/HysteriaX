"""Owned full server processes behind a non-sticky HTTP proxy for live tests.

No request, including a failed write, is automatically replayed by this proxy.
The caller owns its database and directory; no shared process is stopped.
"""
import http.server
import json
from pathlib import Path
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


class MultiInstanceServer:
    def __init__(self, binary, environment, directory, instances=1, migration_log=None):
        if instances not in (1, 3):
            raise ValueError("live verification uses one or three instances")
        self.processes, self.outputs, self.hits = [], [], {}
        self.proxy = None
        self.lock = threading.Lock()
        self.next = 0
        self.ports = []
        self.environment = dict(environment)
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        self.binary = str(binary)
        front_port = int(environment["HYSTERIAX_LISTEN_ADDR"].rsplit(":", 1)[1])
        try:
            subprocess.run([self.binary, "--migrate-only"], env=environment,
                           stdout=migration_log, stderr=subprocess.STDOUT, check=True, timeout=120)
            for index in range(instances):
                port = free_port()
                output = (self.directory / f"instance-{index}.log").open("wb")
                self.outputs.append(output)
                process = subprocess.Popen([self.binary], env={**environment, "HYSTERIAX_LISTEN_ADDR": f"0.0.0.0:{port}"},
                                           stdout=output, stderr=subprocess.STDOUT)
                self.processes.append(process)
                self.ports.append(port)
                deadline = time.monotonic() + 30
                while True:
                    if process.poll() is not None:
                        raise RuntimeError("owned service exited during startup; inspect isolated logs")
                    try:
                        with urllib.request.urlopen(f"http://127.0.0.1:{port}/readyz", timeout=2) as response:
                            if response.status == 200:
                                break
                    except (OSError, urllib.error.URLError):
                        pass
                    if time.monotonic() > deadline:
                        raise TimeoutError("owned service readiness")
                    time.sleep(0.1)
            fixture = self

            class Proxy(http.server.BaseHTTPRequestHandler):
                def log_message(self, *_):
                    pass

                def dispatch(self):
                    with fixture.lock:
                        port = fixture.ports[fixture.next % len(fixture.ports)]
                        fixture.next += 1
                        fixture.hits[str(port)] = fixture.hits.get(str(port), 0) + 1
                    body = self.rfile.read(int(self.headers.get("Content-Length", "0"))) or None
                    excluded = {"host", "connection", "transfer-encoding", "content-length", "proxy-connection"}
                    headers = {key: value for key, value in self.headers.items() if key.lower() not in excluded}
                    request = urllib.request.Request(f"http://127.0.0.1:{port}{self.path}", data=body,
                                                     method=self.command, headers=headers)
                    try:
                        try:
                            upstream = urllib.request.urlopen(request, timeout=45)
                        except urllib.error.HTTPError as error:
                            upstream = error
                    except (OSError, urllib.error.URLError):
                        self.send_response(502)
                        self.send_header("Content-Length", "0")
                        self.end_headers()
                        return
                    with upstream:
                        stream = upstream.headers.get("Content-Type", "").startswith("text/event-stream")
                        self.send_response(upstream.code)
                        for key, value in upstream.headers.items():
                            if key.lower() not in {"connection", "transfer-encoding", "content-length"}:
                                self.send_header(key, value)
                        if stream:
                            self.end_headers()
                            try:
                                while chunk := upstream.read1(65536):
                                    self.wfile.write(chunk)
                                    self.wfile.flush()
                            except (OSError, TimeoutError):
                                pass
                        else:
                            data = upstream.read()
                            self.send_header("Content-Length", str(len(data)))
                            self.end_headers()
                            try:
                                self.wfile.write(data)
                            except (BrokenPipeError, ConnectionResetError):
                                pass

                do_GET = dispatch
                do_POST = dispatch
                do_PATCH = dispatch
                do_PUT = dispatch
                do_DELETE = dispatch

            self.proxy = http.server.ThreadingHTTPServer(("0.0.0.0", front_port), Proxy)
            self.proxy.daemon_threads = True
            threading.Thread(target=self.proxy.serve_forever, daemon=True).start()
        except BaseException:
            self.kill()
            self.wait(timeout=10)
            raise

    def poll(self):
        return next((p.returncode for p in self.processes if p.poll() is not None), None)

    def _close_proxy(self):
        if self.proxy:
            self.proxy.shutdown()
            self.proxy.server_close()
            self.proxy = None

    def terminate(self):
        self._close_proxy()
        for process in self.processes:
            if process.poll() is None:
                process.terminate()

    def kill(self):
        self._close_proxy()
        for process in self.processes:
            if process.poll() is None:
                process.kill()

    def wait(self, timeout=None):
        deadline = None if timeout is None else time.monotonic() + timeout
        for process in self.processes:
            process.wait(timeout=None if deadline is None else max(0.01, deadline-time.monotonic()))
        for output in self.outputs:
            output.close()
        return max((p.returncode for p in self.processes), default=0)

    def report(self):
        with self.lock:
            return {"full_service_processes": len(self.processes), "round_robin_hits": dict(self.hits)}
