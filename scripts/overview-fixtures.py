#!/usr/bin/env python3
"""Deterministic local dashboard fixtures; no account or network access."""
import datetime as dt
import json
from pathlib import Path
import sys

folder = Path(sys.argv[1])
folder.mkdir(parents=True, exist_ok=True)
now = dt.datetime.now(dt.timezone.utc)
stamp = now.isoformat()
package = {"expires_at": stamp, "quota_bytes": 100_000_000_000, "cycle": "monthly", "reset_day": 1, "timezone": "Asia/Shanghai", "interface": None, "direction": "both", "expiry_warning_days": 7, "traffic_warning_percent": 80}
usage = {"usage_bytes": 85_000_000_000, "restricted": False, "reasons": [], "next_reset_at": stamp, "interface": "eth0", "sampled_at": stamp, "gap_reason": None, "freshness": "fresh", "alerts": [{"id": "a1", "kind": "traffic_warning", "created_at": stamp}]}
nodes = [{"id": "node-1", "name": "东京主节点", "revision": 3, "deployed_revision": 3, "state": "deployed", "package": package, "package_usage": usage, "last_sample_at": stamp, "data_freshness": "fresh"}]
monitor = {"node_id": "node-1", "name": "东京主节点", "deployment_state": "deployed", "online_status": "fresh", "online_users": 4, "connections": 6, "online_sampled_at": stamp, "probe_status": "failed", "inlet_status": "ok", "probe_sampled_at": stamp, "latency_ms": 123.4, "connection_ms": 450.0, "external_status": "failed", "reason": "外部探测目标返回错误"}
issues = [{"id": "node:node-1:proxy_probe", "entity_type": "node", "entity_id": "node-1", "name": "东京主节点", "kind": "proxy_probe", "severity": 1, "reason": "外部目标持续探测失败", "occurred_at": stamp}]
overview = {"generated_at": stamp, "node_count": 1, "node_states": {"deployed": 1}, "attention_nodes": 1, "risk_nodes": 1, "queued_jobs": 1, "running_jobs": 2, "failed_jobs_24h": 3, "online_users": 4, "connections": 6, "eligible_nodes": 1, "covered_nodes": 1, "issues": issues, "nodes": [monitor], "quota_rank": nodes}
buckets = []
for index in range(7):
    at = now - dt.timedelta(days=7-index)
    buckets.append({"start": at.isoformat(), "end": (at+dt.timedelta(days=1)).isoformat(), "tx_bytes": None if index == 0 else index*1_000_000_000, "rx_bytes": None if index == 0 else index*2_000_000_000, "incomplete": index == 0, "online_incomplete": index == 0, "traffic_covered_nodes": 0 if index == 0 else 1, "traffic_expected_nodes": 1, "missing_reason": "无采样" if index == 0 else None, "online_users_avg": None if index == 0 else float(index), "online_users_peak": None if index == 0 else float(index+2), "connections_avg": None if index == 0 else float(index*2), "connections_peak": None if index == 0 else float(index*2+3), "covered_nodes": 0 if index == 0 else 1, "probe_attempts": 0 if index == 0 else 20, "probe_successes": 0 if index == 0 else 18, "latency_p50_ms": None if index == 0 else 100.0+index*5, "latency_p95_ms": None if index == 0 else 200.0+index*10})
history = {"generated_at": stamp, "range": "7d", "timezone": "Asia/Shanghai", "source": "users", "buckets": buckets}
for name, value in [("nodes", nodes), ("overview", overview), ("history", history)]:
    (folder / f"{name}.json").write_text(json.dumps(value, ensure_ascii=False))

# A rolling 24-hour fixture matches the default picker and exact chart bounds.
begin = now - dt.timedelta(hours=24)
recent_buckets = []
for index in range(24):
    template = dict(buckets[min(index, 6)])
    at = begin + dt.timedelta(hours=index)
    template["start"] = at.isoformat()
    template["end"] = (at + dt.timedelta(hours=1)).isoformat()
    recent_buckets.append(template)
recent = dict(history, range="24h", buckets=recent_buckets)
(folder / "history-24h.json").write_text(json.dumps(recent, ensure_ascii=False))

# Longer history exercises narrow bars, missing periods and successful zero traffic.
month_buckets = []
for index in range(30):
    template = dict(buckets[min(index, 6)])
    at = now - dt.timedelta(days=30-index)
    template.update(start=at.isoformat(), end=(at+dt.timedelta(days=1)).isoformat())
    if index == 10:
        template.update(tx_bytes=0, rx_bytes=0, incomplete=False)
    month_buckets.append(template)
(folder / "history-30d.json").write_text(json.dumps(dict(history, range="30d", buckets=month_buckets), ensure_ascii=False))

users = [{"id": "user-1", "name": "测试到期用户", "enabled": True, "expires_at": (now-dt.timedelta(days=1)).isoformat(), "quota_bytes": 1000000, "usage_bytes": 1000000, "revision": 1, "assignments": [], "created_at": stamp, "updated_at": stamp}]
jobs = [{"id": "job-1", "kind": "sync", "node_id": "node-1", "node_name": "东京主节点", "status": "failed", "stage": "failed", "attempts": 1, "error_message": "测试任务失败原因", "created_at": stamp, "updated_at": stamp, "finished_at": stamp}]
jobs[0]["retry_job_id"] = "job-2"
jobs.append(dict(jobs[0], id="job-2", retry_of_job_id="job-1", retry_job_id=None, error_message="重试后的最新失败原因"))
for name, value in [("users", users), ("jobs", jobs)]:
    (folder / f"{name}.json").write_text(json.dumps(value, ensure_ascii=False))

server = {"service_version": "0.1.0", "service_uptime_seconds": 86400, "database": "ok", "sampled_at": stamp, "hostname": "management-server", "os": "Debian GNU/Linux 12", "host_uptime_seconds": 259200, "cpu_count": 4, "cpu_usage_percent": 18.5, "memory_used_bytes": 2_000_000_000, "memory_total_bytes": 8_000_000_000, "root_disk_used_bytes": 30_000_000_000, "root_disk_total_bytes": 100_000_000_000}
(folder / "server-monitoring.json").write_text(json.dumps(server, ensure_ascii=False))
