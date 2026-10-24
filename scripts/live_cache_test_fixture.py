"""Minimal bounded RESP client and privately owned Redis for fault verification."""
import socket
import subprocess
import time
import uuid


class Redis:
    def __init__(self, port):
        self.port = port

    def command(self, *arguments):
        parts = [v if isinstance(v, bytes) else str(v).encode() for v in arguments]
        wire = b'*%d\r\n' % len(parts) + b''.join(b'$%d\r\n' % len(v) + v + b'\r\n' for v in parts)
        with socket.create_connection(('127.0.0.1', self.port), timeout=3) as connection:
            connection.sendall(wire)
            with connection.makefile('rb') as stream:
                def read():
                    line = stream.readline(65537)
                    if not line.endswith(b'\r\n') or len(line) > 65536:
                        raise AssertionError('invalid Redis test response')
                    kind, value = line[:1], line[1:-2]
                    if kind == b'+':
                        return value
                    if kind == b':':
                        return int(value)
                    if kind == b'$':
                        size = int(value)
                        if size == -1:
                            return None
                        assert 0 <= size <= 8 * 1024 * 1024
                        data = stream.read(size)
                        assert len(data) == size and stream.read(2) == b'\r\n'
                        return data
                    if kind == b'*':
                        size = int(value)
                        assert 0 <= size <= 10000
                        return [read() for _ in range(size)]
                    raise AssertionError('Redis command failed (details withheld)')
                return read()

    def ready(self):
        try:
            return self.command('PING') == b'PONG'
        except (OSError, AssertionError):
            return False

    def keys(self, pattern):
        cursor, keys = 0, []
        for _ in range(100):
            cursor, page = self.command('SCAN', cursor, 'MATCH', pattern, 'COUNT', 100)
            keys.extend(page)
            if cursor == b'0':
                return sorted(set(keys))
        raise AssertionError('Redis test key scan exceeded bound')


class OwnedRedis(Redis):
    def __init__(self, image, port):
        super().__init__(port)
        self.name = 'hx0034-cache-' + uuid.uuid4().hex
        self.created = False
        subprocess.run(['docker', 'create', '--name', self.name, '-p', f'127.0.0.1:{port}:6379',
                        image, 'redis-server', '--save', '', '--appendonly', 'no'], check=True, stdout=subprocess.DEVNULL)
        self.created = True
        try:
            self.start()
        except BaseException:
            self.close()
            raise

    def start(self):
        subprocess.run(['docker', 'start', self.name], check=True, stdout=subprocess.DEVNULL)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.ready():
                return
            time.sleep(.05)
        raise AssertionError('owned Redis did not become ready')

    def stop(self):
        subprocess.run(['docker', 'stop', '-t', '2', self.name], check=True, stdout=subprocess.DEVNULL)

    def close(self):
        if self.created:
            subprocess.run(['docker', 'rm', '-f', self.name], check=True, stdout=subprocess.DEVNULL)
            self.created = False
