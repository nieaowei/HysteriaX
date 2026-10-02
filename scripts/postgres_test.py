"""Temporary PostgreSQL schemas shared by live acceptance scripts."""

import os
import uuid
from urllib.parse import parse_qsl, quote, urlencode, urlsplit, urlunsplit

import psycopg
from psycopg import sql


class PostgresTestSchema:
    def __init__(self, admin_url=None):
        self.admin_url = admin_url or os.environ.get("TEST_DATABASE_URL")
        if not self.admin_url:
            raise RuntimeError("TEST_DATABASE_URL must point to a disposable PostgreSQL database")
        self.name = f"hysteriax_test_{uuid.uuid4().hex[:20]}"
        self.url = None

    def __enter__(self):
        with psycopg.connect(self.admin_url, autocommit=True) as connection:
            connection.execute(
                sql.SQL("CREATE SCHEMA {}").format(sql.Identifier(self.name))
            )
        parts = urlsplit(self.admin_url)
        query = parse_qsl(parts.query, keep_blank_values=True)
        query.append(("options", f"-c search_path={self.name}"))
        self.url = urlunsplit(
            (parts.scheme, parts.netloc, parts.path, urlencode(query, quote_via=quote), parts.fragment)
        )
        return self

    def connect(self):
        if self.url is None:
            raise RuntimeError("temporary PostgreSQL schema has not been entered")
        return psycopg.connect(self.url)

    def __exit__(self, _type, _value, _traceback):
        with psycopg.connect(self.admin_url, autocommit=True) as connection:
            connection.execute(
                sql.SQL("DROP SCHEMA IF EXISTS {} CASCADE").format(
                    sql.Identifier(self.name)
                )
            )
