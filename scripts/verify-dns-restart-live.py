#!/usr/bin/env python3
"""Verify a persisted DNS task across a service restart (optionally an image upgrade)."""
import argparse
import importlib.util
import json
from pathlib import Path
import re
import shlex
import subprocess
import time
import urllib.error
import uuid

ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("dns_live", ROOT / "scripts/verify-dns-live.py")
live = importlib.util.module_from_spec(spec)
spec.loader.exec_module(live)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="https://hysteriax.cc")
    parser.add_argument("--zone", required=True)
    parser.add_argument("--ssh-host", required=True)
    parser.add_argument("--ssh-key", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--image", help="locally built/pushed image to deploy during restart")
    parser.add_argument("--compose-dir", default="/home/nieaowei/HysteriaX")
    args = parser.parse_args()
    api = live.API(args.base_url, live.environment()["HYSTERIAX_ADMIN_TOKEN"])
    zone = next(entry for entry in api.get("/api/v1/dns/zones") if entry["name"] == args.zone and entry["enabled"])
    name = "hysteriax-dns-test-restart-" + uuid.uuid4().hex[:8] + "." + args.zone
    ssh = ["ssh","-i",str(args.ssh_key.expanduser()),"-o","BatchMode=yes","-o","ConnectTimeout=10",args.ssh_host]
    suffix = uuid.uuid4().hex[:8]
    fixture_name = "hysteriax_dns_delay_" + suffix
    literal_name = "'" + name.replace("'", "''") + "'"
    # A scoped fixture trigger schedules this single write inside its enqueue
    # transaction. Delaying it after the HTTP response races a fast worker.
    delay_sql = f"""CREATE FUNCTION {fixture_name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN
      IF NEW.kind='dns-record-create' AND NEW.resource_name={literal_name}
         AND OLD.resource_name IS DISTINCT FROM NEW.resource_name THEN
        NEW.available_at=now()+interval '45 seconds';
      END IF;
      RETURN NEW;
    END $$;
    CREATE TRIGGER {fixture_name} BEFORE UPDATE ON jobs FOR EACH ROW EXECUTE FUNCTION {fixture_name}();"""
    def database(sql):
        return subprocess.run(ssh+["docker exec hysteriax-postgres-1 psql -v ON_ERROR_STOP=1 -U postgres -d hysteriax -At -c "+shlex.quote(sql)],capture_output=True,text=True,check=True)
    database(delay_sql)
    try:
        receipt = api.write("POST", "/api/v1/dns/records", {"zone_id":zone["id"], "idempotency_key":str(uuid.uuid4()),
            "record":{"name":name,"record_type":"A","content":"35.212.233.15","ttl":1,"proxied":False}})
    finally:
        database(f"DROP TRIGGER {fixture_name} ON jobs; DROP FUNCTION {fixture_name}();")
    identifier = str(uuid.UUID(receipt["job_id"]))
    manifest = {"record_id":receipt["resource_id"],"job_id":identifier,"name":name}
    args.manifest.write_text(json.dumps(manifest,indent=2))
    args.manifest.chmod(0o600)
    result = database(f"SELECT status FROM jobs WHERE id='{identifier}';")
    assert result.stdout.strip() == "queued", "test setup failed: write was not queued at restart"
    manifest["before_restart"] = result.stdout.strip()
    print("Persisted test task:",manifest["before_restart"],flush=True)
    if args.image:
        if not re.fullmatch(r"nieaowei/hysteriax-server:[a-zA-Z0-9_.-]+",args.image):
            raise RuntimeError("expected the locally built HysteriaX registry image")
        repository,tag=args.image.rsplit(":",1)
        update = """from pathlib import Path
p=Path('.env')
updates=%r
lines=[]
for line in p.read_text().splitlines():
    key=line.split('=',1)[0]
    lines.append(key+'='+updates.pop(key) if key in updates else line)
lines.extend(key+'='+value for key,value in updates.items())
p.write_text('\\n'.join(lines)+'\\n')
p.chmod(0o600)
""" % {"HYSTERIAX_IMAGE":repository,"HYSTERIAX_VERSION":tag}
        subprocess.run(ssh+["cd "+shlex.quote(args.compose_dir)+" && python3 -"],input=update,text=True,check=True)
        subprocess.run(ssh+["cd "+shlex.quote(args.compose_dir)+" && docker pull "+shlex.quote(args.image)+" && docker compose up -d --no-build --no-deps --pull never api"],check=True)
    else:
        subprocess.run(ssh+["docker restart hysteriax-api-1"],check=True)
    deadline=time.monotonic()+60
    while time.monotonic()<deadline:
        try:
            if api.get("/readyz").get("status")=="ready":break
        except (RuntimeError,urllib.error.URLError):pass
        time.sleep(1)
    else:raise RuntimeError("service did not become ready after restart")
    api.job(identifier)
    record=api.get("/api/v1/dns/records/"+receipt["resource_id"])
    assert record["state"]=="synced"
    matches=[entry for entry in api.get("/api/v1/dns/records") if entry["name"]==name]
    assert len(matches)==1 and matches[0]["provider_record_id"]
    deletion=api.write("DELETE","/api/v1/dns/records/"+record["id"],live.action(record["revision"]))
    api.job(deletion["job_id"])
    assert api.get("/api/v1/dns/records/"+record["id"])["state"]=="deleted"
    manifest.update({"verification_complete":True,"cleanup_complete":True})
    args.manifest.write_text(json.dumps(manifest,indent=2));args.manifest.chmod(0o600)
    print("PASS: queued DNS write survived service restart, produced one record, and was cleaned up",flush=True)


if __name__=="__main__":main()
