"""Bounded private diagnostics for a caller-owned live-test database only."""
import datetime
import json
import os
from pathlib import Path
import threading
import time

import psycopg


class DatabaseDiagnostics:
    def __init__(self, database_url, destination):
        self.destination = Path(destination)
        self.stop_event = threading.Event()
        self.samples = 0
        self.errors = 0
        self.thread = threading.Thread(target=self._run, args=(database_url,), daemon=True)
        self.thread.start()

    def _run(self, database_url):
        try:
            descriptor = os.open(self.destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "w") as output, psycopg.connect(database_url, autocommit=True, connect_timeout=5) as connection:
                connection.execute("SET statement_timeout='2s'")
                while not self.stop_event.is_set() and self.samples < 1800:
                    activity = connection.execute("SELECT pid,state,wait_event_type,wait_event,coalesce(extract(epoch FROM clock_timestamp()-xact_start),0)::float8,coalesce(extract(epoch FROM clock_timestamp()-query_start),0)::float8,pg_blocking_pids(pid),left(query,240) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND backend_type='client backend' ORDER BY pid").fetchall()
                    suspicious = any(row[6] or row[4] > 1 or (row[1] == "active" and row[5] > 1) for row in activity)
                    if suspicious or self.samples % 5 == 0:
                        locks = connection.execute("SELECT pid,locktype,coalesce(relation::regclass::text,''),mode,granted FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND pid<>pg_backend_pid() ORDER BY pid,locktype,relation,mode").fetchall() if suspicious else []
                        output.write(json.dumps({"at": datetime.datetime.now(datetime.timezone.utc).isoformat(), "activity": activity, "locks": locks}) + "\n")
                        output.flush()
                    self.samples += 1
                    self.stop_event.wait(0.5)
        except Exception:
            # Do not print connection strings or SQL error details to reports.
            self.errors += 1

    def stop(self):
        self.stop_event.set()
        self.thread.join(timeout=5)
        return {"samples": self.samples, "errors": self.errors, "stopped": not self.thread.is_alive()}
