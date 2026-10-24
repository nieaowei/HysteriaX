"""Evidence for intentional metadata changes on a caller-owned migration clone.

Reference aggregates are computed in Python from raw samples, without invoking
production refresh functions. No sample payload is written into the report.
"""
from collections import defaultdict
from datetime import timedelta
from decimal import Decimal
import uuid

from psycopg import sql

REVIEWED = {'_sqlx_migrations','analytics_dirty_cycles','batch_cache_versions',
            'cache_domain_versions','cache_generation','online_cycle_metrics',
            'online_minute_totals','online_dirty_minutes','traffic_dirty_hours','traffic_hour_totals'}


def prepare(connection):
    schema = 'migration_audit_' + uuid.uuid4().hex
    connection.execute(sql.SQL('CREATE SCHEMA {}').format(sql.Identifier(schema)))
    for table in REVIEWED:
        connection.execute(sql.SQL('CREATE TABLE {} AS TABLE {}').format(
            sql.Identifier(schema, table), sql.Identifier('public', table)))
    return schema


def cycle_values(samples):
    """Match documented successful/failed/zero and cross-node distinct semantics."""
    grouped = defaultdict(list)
    for cycle,node,started,status,traffic,connections,expected in samples:
        grouped[cycle].append((node,started,status,traffic,connections,expected))
    result = {}
    for cycle, values in grouped.items():
        successful = [value for value in values if value[2]=='ok']
        users = {user for value in successful for user in value[4]}
        connections = sum((Decimal(str(count)) for value in successful for count in value[4].values()), Decimal(0))
        covered = len(successful)
        expected = max(value[5] for value in values)
        traffic_covered = sum(value[3]=='ok' for value in values)
        result[(cycle,'')] = (cycle,'',min(value[1] for value in values),len(users) if successful else None,
                              connections if successful else None,covered,expected,
                              any(value[2]!='ok' for value in values) or covered<expected,
                              traffic_covered,traffic_covered<expected)
        for node,started,status,traffic,counts,_ in values:
            result[(cycle,node)] = (cycle,node,started,len(counts) if status=='ok' else None,
                                   sum((Decimal(str(count)) for count in counts.values()),Decimal(0)) if status=='ok' else None,
                                   int(status=='ok'),1,status!='ok',int(traffic=='ok'),traffic!='ok')
    return result


def samples_for(connection, cycles):
    return connection.execute('SELECT cycle_id,node_id,cycle_started_at,status,traffic_status,connections,expected_nodes FROM online_samples WHERE cycle_id=ANY(%s)',(list(cycles),)).fetchall() if cycles else []


def check(connection, schema, changed_tables):
    checks = {}
    counts = {}
    def verify(name, condition):
        checks[name] = bool(condition)
        if not condition:
            raise AssertionError('migration evidence failed: '+name)
    def old(table):
        return sql.Identifier(schema,table)
    def changed_keys(table, keys):
        columns = sql.SQL(',').join(sql.Identifier(key) for key in keys)
        selection = sql.SQL(',').join(sql.SQL('coalesce(a.{0},b.{0})').format(sql.Identifier(key)) for key in keys)
        return set(connection.execute(sql.SQL('SELECT {} FROM {} a FULL JOIN {} b USING({}) WHERE to_jsonb(a) IS DISTINCT FROM to_jsonb(b)').format(selection,sql.Identifier('public',table),old(table),columns)).fetchall())

    verify('no_unclassified_table_changes',set(changed_tables)<=REVIEWED)
    mismatch = connection.execute(sql.SQL('SELECT count(*) FROM ((SELECT * FROM {} EXCEPT ALL SELECT m.* FROM public._sqlx_migrations m JOIN {} b USING(version)) UNION ALL (SELECT m.* FROM public._sqlx_migrations m JOIN {} b USING(version) EXCEPT ALL SELECT * FROM {})) difference').format(old('_sqlx_migrations'),old('_sqlx_migrations'),old('_sqlx_migrations'),old('_sqlx_migrations'))).fetchone()[0]
    verify('original_migration_records_unchanged',mismatch==0)
    domains = {row[0] for row in connection.execute(sql.SQL('SELECT domain FROM {}').format(old('cache_domain_versions')))}|{'directory'}
    buckets = connection.execute('SELECT domain,count(*),min(bucket),max(bucket) FROM cache_bucket_versions GROUP BY domain').fetchall()
    verify('all_original_domains_have_exactly_64_buckets',set(row[0] for row in buckets)==domains and all(row[1:]==(64,0,63) for row in buckets))
    verify('retired_global_version_tables_removed',connection.execute("SELECT to_regclass('public.cache_generation') IS NULL AND to_regclass('public.cache_domain_versions') IS NULL").fetchone()[0])
    expected_batches = set(connection.execute(sql.SQL('SELECT b.id,coalesce(v.generation,1) FROM credential_batches b LEFT JOIN {} v ON v.batch_id=b.id').format(old('batch_cache_versions'))).fetchall())
    actual_batches = set(connection.execute('SELECT batch_id,generation FROM batch_cache_versions').fetchall())
    verify('batch_compatibility_view_preserves_live_entities_and_versions',actual_batches==expected_batches)
    counts['orphan_batch_cache_rows_removed'] = connection.execute(sql.SQL('SELECT count(*) FROM {} v WHERE NOT EXISTS(SELECT 1 FROM credential_batches b WHERE b.id=v.batch_id)').format(old('batch_cache_versions'))).fetchone()[0]

    dirty_cycles = {row[0] for row in connection.execute(sql.SQL('SELECT cycle_id FROM {}').format(old('analytics_dirty_cycles')))}
    cycles_changed = changed_keys('online_cycle_metrics',('cycle_id','scope_node_id'))
    verify('cycle_changes_confined_to_original_dirty_keys',all(key[0] in dirty_cycles for key in cycles_changed))
    reference = cycle_values(samples_for(connection,dirty_cycles))
    actual = {tuple(row[:2]):tuple(row) for row in connection.execute('SELECT cycle_id,scope_node_id,started_at,users,connections,covered,expected,incomplete,traffic_covered,traffic_incomplete FROM online_cycle_metrics WHERE cycle_id=ANY(%s)',(list(dirty_cycles),))}
    verify('refreshed_cycles_match_raw_sample_reference',actual==reference)
    counts.update(original_dirty_cycles=len(dirty_cycles),changed_cycle_rows=len(cycles_changed))

    minute_keys = set(connection.execute(sql.SQL('SELECT scope_node_id,minute FROM {}').format(old('online_dirty_minutes'))).fetchall())
    # Both removed old metrics and inserted/rebuilt metrics can dirty a minute.
    for value in reference.values():minute_keys.add((value[1],value[2].replace(second=0,microsecond=0)))
    previous = connection.execute(sql.SQL('SELECT scope_node_id,started_at FROM {} WHERE cycle_id=ANY(%s)').format(old('online_cycle_metrics')),(list(dirty_cycles),)).fetchall()
    for scope,started in previous:minute_keys.add((scope,started.replace(second=0,microsecond=0)))
    minute_changes = changed_keys('online_minute_totals',('scope_node_id','minute'))
    verify('minute_changes_confined_to_refreshed_cycle_minutes',minute_changes<=minute_keys)
    for scope,minute in minute_keys:
        cycle_ids = [row[0] for row in connection.execute("SELECT DISTINCT cycle_id FROM online_samples WHERE cycle_started_at>=%s AND cycle_started_at<%s AND (%s='' OR node_id=%s)",(minute,minute+timedelta(minutes=1),scope,scope))]
        values = [value for value in cycle_values(samples_for(connection,cycle_ids)).values() if value[1]==scope and minute<=value[2]<minute+timedelta(minutes=1)]
        actual = connection.execute('SELECT * FROM online_minute_totals WHERE scope_node_id=%s AND minute=%s',(scope,minute)).fetchall()
        user_values = [value[3] for value in values if value[3] is not None]
        connections = [value[4] for value in values if value[4] is not None]
        expected = [(scope,minute,len(values),len(user_values),sum(user_values),max(user_values,default=None),len(connections),sum(connections),max(connections,default=None),min(value[5] for value in values),max(value[6] for value in values),any(value[7] for value in values),min(value[8] for value in values),any(value[9] for value in values))] if values else []
        verify('online_minutes_match_raw_sample_reference',actual==expected)
    counts['changed_online_minutes'] = len(minute_changes)

    hours = set(connection.execute(sql.SQL('SELECT source,node_id,hour FROM {}').format(old('traffic_dirty_hours'))).fetchall())
    hour_changes = changed_keys('traffic_hour_totals',('source','node_id','hour'))
    verify('hour_changes_confined_to_original_dirty_keys',hour_changes<=hours)
    for source,node,hour in hours:
        source_table = {'users':'traffic_records','network':'node_network_samples'}[source]
        values = connection.execute(sql.SQL('SELECT delta_tx,delta_rx,gap_reason,baseline_only FROM {} WHERE node_id=%s AND sampled_at>=%s AND sampled_at<%s').format(sql.Identifier('public',source_table)),(node,hour,hour+timedelta(hours=1))).fetchall()
        actual = connection.execute('SELECT * FROM traffic_hour_totals WHERE source=%s AND node_id=%s AND hour=%s',(source,node,hour)).fetchall()
        expected = [(source,node,hour,sum(row[0] for row in values),sum(row[1] for row in values),len(values),sum(row[2] is not None or row[3] for row in values))] if values else []
        verify('traffic_hours_match_raw_sample_reference',actual==expected)
    counts.update(original_dirty_hours=len(hours),changed_traffic_hours=len(hour_changes))
    verify('original_pending_refresh_work_fully_acknowledged',connection.execute('SELECT NOT EXISTS(SELECT 1 FROM analytics_dirty_cycles) AND NOT EXISTS(SELECT 1 FROM traffic_dirty_hours) AND NOT EXISTS(SELECT 1 FROM online_dirty_minutes)').fetchone()[0])
    source_count,published,missing,lowest,highest,head = connection.execute("SELECT (SELECT count(*) FROM job_events),(SELECT count(*) FROM event_publications),(SELECT count(*) FROM job_events e WHERE NOT EXISTS(SELECT 1 FROM event_publications p WHERE p.event_id=e.id)),(SELECT coalesce(min(publication_seq),0) FROM event_publications),(SELECT coalesce(max(publication_seq),0) FROM event_publications),(SELECT head FROM event_publication_stream WHERE id)").fetchone()
    verify('all_historical_source_events_have_contiguous_publications',source_count==published and missing==0 and lowest==(1 if source_count else 0) and highest==source_count and head==highest)
    counts['backfilled_historical_publications'] = published
    verify('all_public_foreign_keys_validated',connection.execute("SELECT NOT EXISTS(SELECT 1 FROM pg_constraint WHERE connamespace='public'::regnamespace AND contype='f' AND NOT convalidated)").fetchone()[0])
    verify('retired_write_lock_function_removed',connection.execute("SELECT NOT EXISTS(SELECT 1 FROM pg_proc WHERE pronamespace='public'::regnamespace AND proname='hysteriax_serialize_writes')").fetchone()[0])
    verify('no_retired_advisory_locks_held',connection.execute("SELECT NOT EXISTS(SELECT 1 FROM pg_locks WHERE database=(SELECT oid FROM pg_database WHERE datname=current_database()) AND locktype='advisory' AND objsubid=1 AND classid::bigint*4294967296+objid::bigint IN (62177345902811,62177345902812))").fetchone()[0])
    return {'checks':checks,'counts':counts}
