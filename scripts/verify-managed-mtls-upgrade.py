#!/usr/bin/env python3
"""Exercise real migration-only upgrades from both pre-0032 credential formats.

Uses two temporary schemas in TEST_DATABASE_URL, applies the actual 0031 SQL
baseline with SQLx checksums, seeds identical business state, upgrades one copy,
and calls the independent preservation/reset verifier. Run without another
HysteriaX API process using this disposable database (single-instance lock).
"""
import base64
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import uuid

from nacl.bindings import crypto_aead_xchacha20poly1305_ietf_encrypt
from postgres_test import PostgresTestSchema

ROOT = Path(__file__).resolve().parent.parent
STAMP = "2026-10-01T00:00:00Z"
CREDENTIAL_IDS = {name:str(uuid.uuid5(uuid.NAMESPACE_URL,"hysteriax-upgrade/"+name)) for name in ("ssh","tls","ca","old-client","other-token")}


def baseline(database):
    with database.connect() as connection:
        connection.execute("CREATE TABLE _sqlx_migrations(version BIGINT PRIMARY KEY,description TEXT NOT NULL,installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),success BOOLEAN NOT NULL,checksum BYTEA NOT NULL,execution_time BIGINT NOT NULL)")
        for path in sorted((ROOT/"crates/hysteriax-server/migrations").glob("*.sql")):
            version = int(path.name.split("_",1)[0])
            if version>31: continue
            data = path.read_bytes()
            connection.execute(data.decode(),prepare=False)
            connection.execute("INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time) VALUES(%s,%s,TRUE,%s,0)",(version,path.stem.split("_",1)[1].replace("_"," "),hashlib.sha384(data).digest()))


def seed(database, converted, encrypted, ca_pem):
    cert = (ROOT/"tests/fixtures/credentials-test.crt").read_text()
    key = (ROOT/"tests/fixtures/credentials-test.key").read_text()
    with database.connect() as c:
        c.execute("INSERT INTO admin_tokens(id,token_hash,label,created_at) VALUES('admin','preserved-admin-hash','Existing admin',%s)",(STAMP,))
        c.execute("INSERT INTO users(id,name,quota_bytes,usage_bytes,created_at,updated_at) VALUES('alice','Alice',100000,300,%s,%s)",(STAMP,STAMP))
        c.execute("INSERT INTO subscription_credentials(id,user_id,token_hash,token_enc,created_at) VALUES('subscription','alice','preserved-subscription-hash',%s,%s)",(encrypted["subscription"],STAMP))
        c.execute("INSERT INTO authorization_groups(id,name,created_at,updated_at) VALUES('group','Existing authorization',%s,%s)",(STAMP,STAMP))
        c.execute("INSERT INTO authorization_group_users(group_id,user_id,created_at) VALUES('group','alice',%s)",(STAMP,))
        if converted:
            material = [("ssh","ssh_password",None,encrypted["ssh-payload"]),("tls","tls_identity",None,encrypted["tls-payload"]),("ca","ca_certificate",None,encrypted["ca-payload"]),("old-client","tls_identity","alice","corrupt-unused-old-client-material"),("other-token","api_token",None,encrypted["other-token"])]
            for identity,kind,owner,payload in material:
                c.execute("INSERT INTO credentials(id,name,kind,owner_user_id,created_at,updated_at) VALUES(%s,%s,%s,%s,%s,%s)",(CREDENTIAL_IDS[identity],identity,kind,owner,STAMP,STAMP))
                c.execute("INSERT INTO credential_versions(credential_id,version,payload_enc,created_at) VALUES(%s,1,%s,%s)",(CREDENTIAL_IDS[identity],payload,STAMP))
        for identity in ("active-target","active-actual","history-only"):
            c.execute("INSERT INTO nodes(id,name,ssh_host,ssh_port,ssh_username,ssh_auth_type,ssh_secret_enc,node_token_hash,node_token_enc,traffic_stats_secret_enc,public_host,public_port,listen_addr,desired_config_enc,deployed_config_enc,deployed_revision,deployed_content_sha256,state,created_at,updated_at) VALUES(%s,%s,'127.0.0.1',22,'root','password',%s,%s,%s,%s,'example.test',443,':443',%s,%s,1,'remote-digest','deployed',%s,%s)",(identity,identity,encrypted["ssh"],identity+"-node-hash",encrypted["node-token"],encrypted["stats"],encrypted[identity+"-desired"],encrypted[identity+"-deployed"],STAMP,STAMP))
            if converted: c.execute("UPDATE nodes SET ssh_credential_id=%s,ssh_credential_version=1 WHERE id=%s",(CREDENTIAL_IDS["ssh"],identity))
            else:
                for resource,kind,content in [("server-cert","certificate",cert),("server-key","private_key",key),("client-ca","certificate",ca_pem)]:
                    c.execute("INSERT INTO config_resources(id,node_id,name,resource_kind,content_enc,content_sha256,size_bytes,created_at) VALUES(%s,%s,%s,%s,%s,%s,%s,%s)",(identity+"-"+resource,identity,resource,kind,encrypted[resource],hashlib.sha256(content.encode()).hexdigest(),len(content.encode()),STAMP))
            c.execute("INSERT INTO config_versions(id,node_id,revision,config_enc,content_sha256,deployed_success,created_at) VALUES(%s,%s,1,%s,%s,TRUE,%s)",(identity+"-history",identity,encrypted[identity+"-history"],encrypted[identity+"-history-hash"],STAMP))
            c.execute("INSERT INTO node_assignments(user_id,node_id,credential_hash,credential_enc,client_certificate_enc,client_private_key_enc,created_at) VALUES('alice',%s,'preserved-password-hash',%s,'corrupt-certificate','corrupt-private-key',%s)",(identity,encrypted["password"],STAMP))
            if converted:c.execute("UPDATE node_assignments SET mtls_credential_id=%s,mtls_credential_version=1 WHERE node_id=%s",(CREDENTIAL_IDS["old-client"],identity))
            c.execute("INSERT INTO authorization_group_nodes(group_id,node_id,created_at) VALUES('group',%s,%s)",(identity,STAMP))
            c.execute("INSERT INTO traffic_records(id,node_id,user_id,instance_id,baseline_tx,baseline_rx,delta_tx,delta_rx,sampled_at) VALUES(%s,%s,'alice','existing-instance',100,200,100,200,%s)",(identity+"-traffic",identity,STAMP))
            c.execute("INSERT INTO traffic_baselines(node_id,user_id,instance_id,tx_total,rx_total,sampled_at) VALUES(%s,'alice','existing-instance',100,200,%s)",(identity,STAMP))
            c.execute("INSERT INTO jobs(id,kind,node_id,target_revision,status,stage,available_at,created_at,updated_at) VALUES(%s,'sync',%s,1,'queued','queued',%s,%s,%s)",(identity+"-sync",identity,STAMP,STAMP,STAMP))
        c.execute("INSERT INTO jobs(id,kind,node_id,payload_json,status,stage,available_at,created_at,updated_at) VALUES('old-apply','credential-apply','active-target','{\"mode\":\"mtls\"}','queued','queued',%s,%s,%s)",(STAMP,STAMP,STAMP))
        c.execute("INSERT INTO jobs(id,kind,node_id,status,stage,available_at,created_at,updated_at) VALUES('unrelated-ssh','ssh-test','history-only','queued','queued',%s,%s,%s)",(STAMP,STAMP,STAMP))
        if converted:
            c.execute("ALTER TABLE nodes DROP COLUMN ssh_secret_enc,DROP COLUMN ssh_passphrase_enc,DROP COLUMN ssh_auth_type,ALTER COLUMN ssh_credential_id SET NOT NULL,ALTER COLUMN ssh_credential_version SET NOT NULL")
            c.execute("ALTER TABLE node_assignments DROP COLUMN client_certificate_enc,DROP COLUMN client_private_key_enc")
            c.execute("INSERT INTO credential_migrations(name,completed_at) VALUES('credentials_v2',%s),('tls_pairs_v1',%s)",(STAMP,STAMP))


def main():
    subprocess.run(["cargo","build","-q","-p","hysteriax-server"],cwd=ROOT,check=True)
    key = os.urandom(32)
    def encrypt(value):
        if not isinstance(value,str):value=json.dumps(value,separators=(",",":"))
        nonce=os.urandom(24)
        return base64.b64encode(nonce+crypto_aead_xchacha20poly1305_ietf_encrypt(value.encode(),None,nonce,key)).decode().rstrip("=")
    with tempfile.TemporaryDirectory(prefix="hx-mtls-upgrade-") as folder:
        ca_path=Path(folder)/"ca.pem"
        subprocess.run(["openssl","req","-x509","-newkey","ec","-pkeyopt","ec_paramgen_curve:P-256","-nodes","-keyout",str(Path(folder)/"ca-key.pem"),"-out",str(ca_path),"-days","365","-subj","/CN=Old public CA","-addext","basicConstraints=critical,CA:TRUE","-addext","keyUsage=critical,keyCertSign,cRLSign"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True)
        ca_pem=ca_path.read_text()
        cert=(ROOT/"tests/fixtures/credentials-test.crt").read_text();server_key=(ROOT/"tests/fixtures/credentials-test.key").read_text()
        for converted in (False,True):
            encrypted={name:encrypt(value) for name,value in {"ssh":"ssh-password","node-token":"node-callback-token","stats":"statistics-secret","password":"user-connection-password","subscription":"existing-subscription-token","server-cert":cert,"server-key":server_key,"client-ca":ca_pem,"ssh-payload":{"secret":"ssh-password"},"tls-payload":{"certificate":cert,"private_key":server_key},"ca-payload":{"content":ca_pem},"other-token":{"token":"unrelated-token"}}.items()}
            for identity in ("active-target","active-actual","history-only"):
                tls=dict(cert=f"credential://{CREDENTIAL_IDS['tls']}/1/certificate",key=f"credential://{CREDENTIAL_IDS['tls']}/1/private_key") if converted else dict(cert=f"resource://{identity}-server-cert",key=f"resource://{identity}-server-key")
                off={"tls":tls};legacy={"tls":dict(tls,clientCA=f"credential://{CREDENTIAL_IDS['ca']}/1/content" if converted else f"resource://{identity}-client-ca")}
                encrypted[identity+"-desired"]=encrypt(legacy if identity=="active-target" else off)
                encrypted[identity+"-deployed"]=encrypt(legacy if identity=="active-actual" else off)
                snapshot=json.dumps({"server_config":legacy,"public_host":"example.test","public_port":443,"listen_addr":":443"},separators=(",",":"))
                encrypted[identity+"-history"]=encrypt(snapshot);encrypted[identity+"-history-hash"]=hashlib.sha256(snapshot.encode()).hexdigest()
            with PostgresTestSchema() as before,PostgresTestSchema() as after:
                for database in (before,after):baseline(database);seed(database,converted,encrypted,ca_pem)
                env=dict(os.environ,DATABASE_URL=after.url,HYSTERIAX_MASTER_KEY=base64.b64encode(key).decode().rstrip("="))
                command=[str(ROOT/"target/debug/hysteriax-server"),"--migrate-only"]
                result=subprocess.run(command,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=60)
                if result.returncode:
                    message=result.stdout.decode(errors="replace")
                    for secret in (after.url,env["HYSTERIAX_MASTER_KEY"],server_key,ca_pem,"ssh-password","user-connection-password","node-callback-token","statistics-secret","existing-subscription-token"):
                        message=message.replace(secret,"[redacted]")
                    raise RuntimeError("migration-only failed: "+message[-4000:])
                result=subprocess.run(command,env=env,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=60)
                assert result.returncode==0,"repeated migration-only failed"
                env.update(HYSTERIAX_MIGRATION_BEFORE_URL=before.url,HYSTERIAX_MIGRATION_AFTER_URL=after.url)
                subprocess.run([sys.executable,str(ROOT/"scripts/verify-credentials-migration.py")],env=env,check=True)
                print("Upgrade source verified:","credential center" if converted else "inline legacy",flush=True)


if __name__=="__main__":main()
