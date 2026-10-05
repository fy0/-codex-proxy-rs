"""纯 Python 校验和专用 PostgreSQL 集成测试；绝不连接应用的生产 URL。"""
from __future__ import annotations

from contextlib import contextmanager
import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
from uuid import uuid4

import migrate

TEST_URL = os.environ.get("CPR_TEST_DATABASE_URL")


def history_for(bundle, kind, count=None):
    return [(e["version"], e["description"], True, e["sha384"])
            for e in bundle[kind][:count]]


class HistoryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bundle = migrate.load_bundle()

    def test_frozen_files_and_all_known_prefixes(self):
        for kind in ("official", "legacy"):
            for count in range(1, len(self.bundle[kind]) + 1):
                with self.subTest(kind=kind, count=count):
                    expected = "shared" if count <= 15 else kind
                    self.assertEqual(migrate.classify_history(
                        history_for(self.bundle, kind, count), self.bundle), expected)
        self.assertEqual(migrate.classify_history([], self.bundle), "empty")

    def test_rejects_unknown_dirty_gapped_and_relabelled_history(self):
        good = history_for(self.bundle, "legacy")
        bad_cases = [good[1:], good[:-2] + good[-1:], good + [(31, "unknown", True, "00")]]
        for index, value in ((1, "wrong description"), (2, False), (3, "00" * 48)):
            changed = good.copy()
            row = list(changed[-1])
            row[index] = value
            changed[-1] = tuple(row)
            bad_cases.append(changed)
        for rows in bad_cases:
            with self.subTest(rows=len(rows)):
                with self.assertRaises(migrate.MigrationError):
                    migrate.classify_history(rows, self.bundle)

    def test_settings_repr_does_not_contain_credentials(self):
        settings = migrate.Settings("postgres://secret-url", "secret-password", Path("/backups"))
        self.assertNotIn("secret", repr(settings))

    def test_default_entrypoint_does_not_open_database(self):
        with patch.dict(os.environ, {migrate.ENABLE_ENV: "0"}), \
                patch.object(migrate, "connect_database") as connect, \
                patch.object(migrate.os, "execvp") as execute:
            self.assertEqual(migrate.main(["/app/bin/codex-proxy-rs"]), 0)
            connect.assert_not_called()
            execute.assert_called_once_with("/app/bin/codex-proxy-rs", ["/app/bin/codex-proxy-rs"])

    def test_invalid_opt_in_is_not_silently_accepted(self):
        with patch.dict(os.environ, {migrate.ENABLE_ENV: "yes"}):
            with self.assertRaises(migrate.MigrationError):
                migrate.main(["--migrate-only"])


@unittest.skipUnless(TEST_URL or os.environ.get("CPR_REQUIRE_MIGRATION_DB_TESTS") == "1",
                     "未设置专用 CPR_TEST_DATABASE_URL；数据库测试未执行")
class PostgreSQLTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not TEST_URL:
            raise RuntimeError("容器验证必须设置专用 CPR_TEST_DATABASE_URL")
        import psycopg2
        from psycopg2.extensions import parse_dsn
        cls.psycopg2 = psycopg2
        cls.bundle = migrate.load_bundle()
        cls.password = parse_dsn(TEST_URL).get("password", "fixture-only")
        cls.admin = psycopg2.connect(TEST_URL)
        cls.admin.autocommit = True

    @classmethod
    def tearDownClass(cls):
        cls.admin.close()

    @contextmanager
    def database(self):
        from psycopg2 import sql
        from psycopg2.extensions import make_dsn
        name = "cpr_upstream_migration_test_" + uuid4().hex
        with self.admin.cursor() as cursor:
            cursor.execute(sql.SQL("CREATE DATABASE {}").format(sql.Identifier(name)))
        connection = None
        try:
            dsn = make_dsn(TEST_URL, dbname=name)
            connection = self.psycopg2.connect(dsn)
            with tempfile.TemporaryDirectory() as directory:
                yield connection, migrate.Settings(dsn, self.password, Path(directory).resolve())
        finally:
            if connection is not None:
                connection.close()
            with self.admin.cursor() as cursor:
                cursor.execute(sql.SQL("DROP DATABASE {} WITH (FORCE)").format(sql.Identifier(name)))

    def fixture(self, connection, kind="legacy", count=30):
        with connection, connection.cursor() as cursor:
            cursor.execute("""CREATE TABLE public._sqlx_migrations (
                version BIGINT PRIMARY KEY, description TEXT NOT NULL,
                installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL,
                checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)""")
            for entry in self.bundle[kind][:count]:
                cursor.execute(entry["sql"])
                cursor.execute("""INSERT INTO public._sqlx_migrations
                    (version,description,success,checksum,execution_time) VALUES (%s,%s,true,%s,0)""",
                    (entry["version"], entry["description"], bytes.fromhex(entry["sha384"])))

    def seed(self, connection, count=30):
        with connection, connection.cursor() as cursor:
            cursor.execute("""
                INSERT INTO admin_users VALUES ('fixture-admin','synthetic-hash',now(),now());
                -- 0001 已创建单例，测试只能更新它，不能重复插入同一主键。
                UPDATE runtime_settings SET config_revision = 17,
                    admin_api_key = 'synthetic-admin-key', updated_at = now() WHERE id = 1;
                INSERT INTO client_api_keys (id,name,key,created_at,updated_at)
                    VALUES ('fixture-key','fixture','sk_' || repeat('x',43),now(),now());
                INSERT INTO provider_accounts
                    (id,provider_kind,name,authentication_kind,provider_credentials_json,
                     has_refresh_token,credential_observed_at,created_at,updated_at)
                    VALUES ('fixture-account','openai','fixture','oauth',
                            '{"accessToken":"synthetic","refreshToken":"synthetic"}',
                            true,now(),now(),now());
                UPDATE provider_accounts SET turn_state_override = 'synthetic-turn-state';
            """)
            if count >= 17:
                cursor.execute("""INSERT INTO account_turn_states
                    (account_id,model,config,turn_state_override)
                    VALUES ('fixture-account','fixture-model','{}','synthetic-state');
                    INSERT INTO account_turn_state_events (account_id,model,event_kind,detail)
                    VALUES ('fixture-account','fixture-model','observation','{"fixture":true}');""")
            if count >= 19:
                cursor.execute("""INSERT INTO account_turn_state_notifications
                    (account_id,model,issued_at,installation,next_attempt_at)
                    VALUES ('fixture-account','fixture-model',1,'{}',2)""")
            if count >= 22:
                cursor.execute("""INSERT INTO openai_routing_cookies
                    (origin,pod,name,value,issued_at,expires_at,observed_at,reported_model)
                    VALUES ('https://fixture.invalid','pod','cookie','synthetic-cookie',1,2,1,'fixture')""")
            if count >= 27:
                cursor.execute("UPDATE provider_accounts SET basispoints_enabled = true")
            if count >= 28:
                cursor.execute("UPDATE runtime_settings SET bps_model_mappings_json = '{\"fixture\":\"model\"}'")
            if count >= 29:
                cursor.execute("UPDATE provider_accounts SET bps_concurrency_limit = 7")

    def schema(self, connection):
        queries = [
            """SELECT table_name,column_name,data_type,udt_name,is_nullable,column_default,
                      character_maximum_length,numeric_precision,numeric_scale,is_identity
               FROM information_schema.columns WHERE table_schema='public'
               ORDER BY table_name,column_name""",
            """SELECT c.relname,k.conname,pg_get_constraintdef(k.oid)
               FROM pg_constraint k JOIN pg_class c ON c.oid=k.conrelid
               JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public'
               ORDER BY c.relname,k.conname""",
            "SELECT indexname,indexdef FROM pg_indexes WHERE schemaname='public' ORDER BY indexname",
            """SELECT c.relname,t.tgname,pg_get_triggerdef(t.oid)
               FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid
               JOIN pg_namespace n ON n.oid=c.relnamespace
               WHERE n.nspname='public' AND NOT t.tgisinternal ORDER BY c.relname,t.tgname""",
            """SELECT p.proname,pg_get_functiondef(p.oid) FROM pg_proc p
               JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='public'
               ORDER BY p.proname""",
            """SELECT sequence_name,data_type,start_value,minimum_value,maximum_value,increment,cycle_option
               FROM information_schema.sequences WHERE sequence_schema='public' ORDER BY sequence_name""",
        ]
        with connection, connection.cursor() as cursor:
            result = []
            for query in queries:
                cursor.execute(query)
                result.append(cursor.fetchall())
            return result

    def snapshot(self, connection):
        from psycopg2 import sql
        with connection, connection.cursor() as cursor:
            result = {}
            for name in migrate.public_tables(cursor):
                cursor.execute(sql.SQL("SELECT to_jsonb(t) FROM public.{} t").format(sql.Identifier(name)))
                result[name] = sorted((row[0] for row in cursor.fetchall()),
                                      key=lambda row: json.dumps(row, sort_keys=True))
            return result

    def assert_retained(self, before, after):
        removed = {"turn_state_override", "basispoints_enabled", "bps_concurrency_limit",
                   "bps_model_mappings_json", "disable_fast"}
        for name, rows in before.items():
            if name in migrate.FORK_TABLES or name == "_sqlx_migrations":
                continue
            self.assertEqual(len(rows), len(after[name]), name)
            if rows:
                fields = set(rows[0]) - removed
                normalize = lambda records: sorted(
                    json.dumps({key: row[key] for key in fields}, sort_keys=True) for row in records)
                self.assertEqual(normalize(rows), normalize(after[name]), name)

    def test_every_legacy_prefix_matches_fresh_official_schema_and_preserves_data(self):
        with self.database() as (official, _):
            self.fixture(official, "official", 21)
            expected = self.schema(official)
            for count in range(16, 31):
                with self.subTest(version=count), self.database() as (connection, settings):
                    self.fixture(connection, count=count)
                    self.seed(connection, count)
                    before = self.snapshot(connection)
                    status, archive = migrate.migrate_database(connection, settings, self.bundle, enabled=True)
                    self.assertEqual(status, "migrated")
                    self.assertTrue(archive.is_file())
                    self.assertEqual(self.schema(connection), expected)
                    self.assert_retained(before, self.snapshot(connection))
                    self.assertEqual(migrate.migrate_database(connection, settings, self.bundle, enabled=True),
                                     ("official", None))
                    self.assertEqual(len(list(settings.backup_dir.glob("*.dump"))), 1)

    def test_backup_restores_all_legacy_data_and_history(self):
        from psycopg2.extensions import make_dsn
        with self.database() as (connection, settings), self.database() as (restored, restore_settings):
            self.fixture(connection)
            self.seed(connection)
            before = self.snapshot(connection)
            schema = self.schema(connection)
            _, archive = migrate.migrate_database(connection, settings, self.bundle, enabled=True)
            receipt = json.loads(archive.with_suffix(".json").read_text(encoding="utf-8"))
            self.assertEqual(receipt["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest())
            if os.name == "posix":
                self.assertEqual(archive.stat().st_mode & 0o777, 0o600)
            environment = {k: v for k, v in os.environ.items() if not k.startswith("PG")}
            environment.update(PGDATABASE=make_dsn(**restored.get_dsn_parameters()),
                               PGPASSWORD=restore_settings.password)
            result = subprocess.run(["pg_restore", "--exit-on-error", "--no-owner", "--no-privileges",
                                     "--dbname", environment["PGDATABASE"], str(archive)],
                                    env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                                    timeout=120, check=False)
            self.assertEqual(result.returncode, 0, "专用测试数据库的备份恢复失败")
            self.assertEqual(self.schema(restored), schema)
            self.assertEqual(self.snapshot(restored), before)

    def test_backup_failure_leaves_original_database_intact(self):
        with self.database() as (connection, settings):
            self.fixture(connection)
            self.seed(connection)
            before, schema = self.snapshot(connection), self.schema(connection)
            with patch.object(migrate, "backup_database", side_effect=migrate.MigrationError("fixture failure")):
                with self.assertRaises(migrate.MigrationError):
                    migrate.migrate_database(connection, settings, self.bundle, enabled=True)
            self.assertEqual(self.schema(connection), schema)
            self.assertEqual(self.snapshot(connection), before)

    def test_official_sql_failure_rolls_back_compensation_and_history(self):
        with self.database() as (connection, settings):
            self.fixture(connection)
            self.seed(connection)
            before, schema = self.snapshot(connection), self.schema(connection)
            broken = copy.deepcopy(self.bundle)
            broken["official"][17]["sql"] += "\nSELECT 1 / 0;"
            with self.assertRaises(self.psycopg2.Error):
                migrate.migrate_database(connection, settings, broken, enabled=True)
            self.assertEqual(self.schema(connection), schema)
            self.assertEqual(self.snapshot(connection), before)
            self.assertEqual(len(list(settings.backup_dir.glob("*.dump"))), 1)

    def test_unknown_history_and_missing_opt_in_never_start_backup(self):
        with self.database() as (connection, settings):
            self.fixture(connection)
            before = self.snapshot(connection)
            with patch.object(migrate, "backup_database") as backup:
                self.assertEqual(migrate.migrate_database(connection, settings, self.bundle,
                                                         enabled=False, check_only=True), ("legacy", None))
                with self.assertRaises(migrate.MigrationError):
                    migrate.migrate_database(connection, settings, self.bundle, enabled=False)
                self.assertEqual(self.snapshot(connection), before)
                with connection, connection.cursor() as cursor:
                    cursor.execute("UPDATE _sqlx_migrations SET checksum = decode('00','hex') WHERE version=30")
                with self.assertRaises(migrate.MigrationError):
                    migrate.migrate_database(connection, settings, self.bundle, enabled=True)
                backup.assert_not_called()

    def test_relabelled_official_history_with_legacy_objects_is_rejected(self):
        with self.database() as (connection, settings):
            self.fixture(connection)
            with connection, connection.cursor() as cursor:
                cursor.execute("DELETE FROM _sqlx_migrations")
                for entry in self.bundle["official"]:
                    cursor.execute("""INSERT INTO _sqlx_migrations
                        (version,description,success,checksum,execution_time) VALUES (%s,%s,true,%s,0)""",
                        (entry["version"], entry["description"], bytes.fromhex(entry["sha384"])))
            with patch.object(migrate, "backup_database") as backup:
                with self.assertRaises(migrate.MigrationError):
                    migrate.migrate_database(connection, settings, self.bundle, enabled=True)
                backup.assert_not_called()

    def test_stop_check_observes_connections_created_after_first_snapshot(self):
        with self.database() as (connection, settings):
            other = None
            try:
                with connection, connection.cursor() as cursor:
                    migrate.require_stopped(cursor)
                    other = self.psycopg2.connect(settings.dsn)
                    with self.assertRaises(migrate.MigrationError):
                        migrate.require_stopped(cursor)
            finally:
                if other is not None:
                    other.close()

    def test_empty_migration_metadata_is_safe_but_unversioned_business_tables_are_not(self):
        with self.database() as (connection, settings):
            self.fixture(connection, count=0)
            before = self.snapshot(connection)
            self.assertEqual(migrate.migrate_database(connection, settings, self.bundle, enabled=True),
                             ("empty", None))
            self.assertEqual(self.snapshot(connection), before)
            with connection, connection.cursor() as cursor:
                cursor.execute("CREATE TABLE unversioned_business_data (id integer)")
            with self.assertRaises(migrate.MigrationError):
                migrate.migrate_database(connection, settings, self.bundle, enabled=True)

    def test_other_client_and_concurrent_migration_are_rejected(self):
        with self.database() as (connection, settings):
            self.fixture(connection)
            other = self.psycopg2.connect(settings.dsn)
            try:
                with patch.object(migrate, "backup_database") as backup:
                    with self.assertRaises(migrate.MigrationError):
                        migrate.migrate_database(connection, settings, self.bundle, enabled=True)
                    with other.cursor() as cursor:
                        cursor.execute("SELECT pg_advisory_xact_lock(7416613174123)")
                    with self.assertRaises(migrate.MigrationError):
                        migrate.migrate_database(connection, settings, self.bundle, enabled=True)
                    backup.assert_not_called()
            finally:
                other.close()

    def test_fresh_shared_and_official_databases_are_not_rewritten(self):
        for count in (0, 15, 21):
            with self.subTest(count=count), self.database() as (connection, settings):
                if count:
                    self.fixture(connection, "official", count)
                before = self.snapshot(connection)
                expected = {0: "empty", 15: "shared", 21: "official"}[count]
                self.assertEqual(migrate.migrate_database(connection, settings, self.bundle, enabled=True),
                                 (expected, None))
                self.assertEqual(self.snapshot(connection), before)
                self.assertFalse(list(settings.backup_dir.glob("*.dump")))


if __name__ == "__main__":
    unittest.main()
