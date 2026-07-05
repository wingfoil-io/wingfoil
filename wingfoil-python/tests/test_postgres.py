"""Tests for the PostgreSQL Python bindings.

Two groups:

- Unit-level construction / marshaling tests (no marker): run by default, with no
  live service. They keep the ``py_postgres`` binding visible in coverage and exercise
  the pyo3 marshaling glue.
- Integration tests (``@pytest.mark.requires_postgres``): deselected by default. The
  postgres integration workflow opts in with ``-m requires_postgres``; without
  PostgreSQL on localhost:5432 they fail loudly rather than skipping.

Local setup:
    docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
"""

import unittest

import pytest

CONN_STR = "host=localhost port=5432 user=postgres password=postgres dbname=postgres"
# Loopback port 1 is reserved and never hosts a service: connect is rejected.
UNREACHABLE = "host=127.0.0.1 port=1 user=postgres dbname=postgres connect_timeout=1"

# 2000-01-01 00:00:00 UTC as Unix seconds — the historical start used for reads.
KDB_EPOCH_SECS = 946684800.0
ONE_DAY = 86400.0


# ---- Unit-level coverage (no live PostgreSQL) ----


class TestPostgresConstruction(unittest.TestCase):
    def test_read_constructs_stream(self):
        from wingfoil import postgres_read

        stream = postgres_read(
            UNREACHABLE,
            "SELECT time, sym FROM trades",
            "time",
            chunk_size=3600,
        )
        self.assertIsNotNone(stream)

    def test_write_method_constructs_node(self):
        from wingfoil import constant

        node = constant({"sym": "A", "price": 1.0, "qty": 1}).postgres_write(
            UNREACHABLE,
            "trades",
            [("sym", "text"), ("price", "float"), ("qty", "int")],
        )
        self.assertIsNotNone(node)


class TestPostgresUnreachable(unittest.TestCase):
    def test_write_single_dict_marshals_then_errors(self):
        # dict_to_write_row runs on the upstream tick; connect then fails.
        from wingfoil import constant

        node = constant({"sym": "A", "price": 1.0, "qty": 1}).postgres_write(
            UNREACHABLE,
            "trades",
            [("sym", "text"), ("price", "float"), ("qty", "int")],
        )
        with self.assertRaises(Exception):
            node.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_write_list_of_dicts_marshals_then_errors(self):
        from wingfoil import constant

        rows = [
            {"sym": "A", "price": 1.0, "qty": 1},
            {"sym": "B", "price": 2.0, "qty": 2},
        ]
        node = constant(rows).postgres_write(
            UNREACHABLE,
            "trades",
            [("sym", "text"), ("price", "float"), ("qty", "int")],
        )
        with self.assertRaises(Exception):
            node.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_write_bad_value_type_errors(self):
        # Fallthrough branch: neither dict nor list is a marshaling error that
        # aborts the run (fail loud, not silent).
        from wingfoil import constant

        node = constant("not a dict").postgres_write(
            UNREACHABLE, "trades", [("sym", "text")]
        )
        with self.assertRaises(Exception):
            node.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_write_missing_key_errors(self):
        # A declared column absent from the dict aborts the run instead of
        # silently inserting NULL.
        from wingfoil import constant

        node = constant({"sym": "A"}).postgres_write(
            UNREACHABLE, "trades", [("sym", "text"), ("price", "float")]
        )
        with self.assertRaises(Exception):
            node.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_write_unsupported_type_errors(self):
        # A typo'd/unsupported declared type aborts the run.
        from wingfoil import constant

        node = constant({"price": 1.0}).postgres_write(
            UNREACHABLE, "trades", [("price", "flaot")]
        )
        with self.assertRaises(Exception):
            node.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_write_wrong_value_type_errors(self):
        # A value that can't extract as the declared type aborts the run.
        from wingfoil import constant

        node = constant({"qty": "not an int"}).postgres_write(
            UNREACHABLE, "trades", [("qty", "long")]
        )
        with self.assertRaises(Exception):
            node.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_read_unreachable_errors(self):
        from wingfoil import postgres_read

        stream = postgres_read(
            UNREACHABLE, "SELECT time, sym FROM trades", "time"
        ).collect()
        with self.assertRaises(Exception):
            stream.run(realtime=False, start=KDB_EPOCH_SECS, duration=ONE_DAY)

    def test_read_zero_chunk_size_errors(self):
        # period must be > 0; a clean error, not a divide-by-zero panic.
        from wingfoil import postgres_read

        stream = postgres_read(
            UNREACHABLE, "SELECT time, sym FROM trades", "time", chunk_size=0
        ).collect()
        with self.assertRaises(Exception):
            stream.run(realtime=False, start=KDB_EPOCH_SECS, duration=ONE_DAY)


class TestPostgresSub(unittest.TestCase):
    def test_sub_constructs_stream(self):
        from wingfoil import postgres_sub

        stream = postgres_sub(
            UNREACHABLE, "SELECT time, sym FROM trades", "time", "trades_feed"
        )
        self.assertIsNotNone(stream)

    def test_sub_rejects_historical_mode(self):
        # postgres_sub is real-time only; bails before connecting.
        from wingfoil import postgres_sub

        stream = postgres_sub(
            UNREACHABLE, "SELECT time, sym FROM trades", "time", "trades_feed"
        ).collect()
        with self.assertRaises(Exception):
            stream.run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

    def test_sub_unreachable_errors(self):
        from wingfoil import postgres_sub

        stream = postgres_sub(
            UNREACHABLE, "SELECT time, sym FROM trades", "time", "trades_feed"
        ).collect()
        with self.assertRaises(Exception):
            stream.run(realtime=True, duration=2.0)

    def test_notify_trigger_sql_shape(self):
        from wingfoil import postgres_notify_trigger_sql

        sql = postgres_notify_trigger_sql("trades", "trades_feed")
        self.assertIn("pg_notify('trades_feed', '')", sql)
        self.assertIn('AFTER INSERT ON "trades"', sql)


# ---- Integration tests (require PostgreSQL on localhost:5432) ----


def _reset_table(sql_rows=()):
    """Create a fresh `py_pg_trades` table and optionally seed rows. Requires psycopg."""
    import psycopg

    with psycopg.connect(CONN_STR, autocommit=True) as conn:
        conn.execute("DROP TABLE IF EXISTS py_pg_trades")
        conn.execute(
            "CREATE TABLE py_pg_trades "
            "(time timestamp, sym text, price float8, qty int8)"
        )
        for ts, sym, price, qty in sql_rows:
            conn.execute(
                "INSERT INTO py_pg_trades VALUES (%s, %s, %s, %s)",
                (ts, sym, price, qty),
            )


@pytest.mark.requires_postgres
class TestPostgresRead(unittest.TestCase):
    def test_read_returns_dicts(self):
        from wingfoil import postgres_read

        _reset_table(
            [
                ("2000-01-01 00:00:01", "AAPL", 100.0, 10),
                ("2000-01-01 00:00:02", "GOOG", 200.0, 20),
                ("2000-01-01 00:00:03", "MSFT", 300.0, 30),
            ]
        )

        stream = postgres_read(
            CONN_STR,
            "SELECT time, sym, price, qty FROM py_pg_trades",
            "time",
            chunk_size=86400,
        ).collect()
        stream.run(realtime=False, start=KDB_EPOCH_SECS, duration=ONE_DAY)
        rows = stream.peek_value()

        self.assertIsInstance(rows, list)
        self.assertEqual(len(rows), 3)
        self.assertIsInstance(rows[0], dict)
        self.assertEqual({r["sym"] for r in rows}, {"AAPL", "GOOG", "MSFT"})
        self.assertEqual(rows[0]["qty"], 10)


@pytest.mark.requires_postgres
class TestPostgresWrite(unittest.TestCase):
    def test_write_round_trip(self):
        import psycopg
        from wingfoil import constant

        _reset_table()

        # The table's qty column is int8, so the declared type is "long".
        constant({"sym": "TEST", "price": 42.0, "qty": 100}).postgres_write(
            CONN_STR,
            "py_pg_trades",
            [("sym", "text"), ("price", "float"), ("qty", "long")],
        ).run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

        with psycopg.connect(CONN_STR) as conn:
            row = conn.execute(
                "SELECT sym, price, qty FROM py_pg_trades"
            ).fetchone()
        self.assertEqual(row[0], "TEST")
        self.assertAlmostEqual(row[1], 42.0)
        self.assertEqual(row[2], 100)

    def test_write_int4_and_null_round_trip(self):
        """"int" binds as int4 (plain `integer` columns work), None writes SQL NULL."""
        import psycopg
        from wingfoil import constant, postgres_read

        with psycopg.connect(CONN_STR, autocommit=True) as conn:
            conn.execute("DROP TABLE IF EXISTS py_pg_int4")
            conn.execute(
                "CREATE TABLE py_pg_int4 (time timestamp, qty integer, note text)"
            )

        constant({"qty": 7, "note": None}).postgres_write(
            CONN_STR,
            "py_pg_int4",
            [("qty", "int"), ("note", "text")],
        ).run(realtime=False, start=KDB_EPOCH_SECS, cycles=1)

        with psycopg.connect(CONN_STR) as conn:
            row = conn.execute("SELECT qty, note FROM py_pg_int4").fetchone()
        self.assertEqual(row[0], 7)
        self.assertIsNone(row[1])

        # Read back through the adapter: int4 dispatch + NULL -> None.
        stream = postgres_read(
            CONN_STR, "SELECT time, qty, note FROM py_pg_int4", "time", chunk_size=86400
        ).collect()
        stream.run(realtime=False, start=KDB_EPOCH_SECS, duration=ONE_DAY)
        rows = stream.peek_value()
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["qty"], 7)
        self.assertIsNone(rows[0]["note"])


@pytest.mark.requires_postgres
class TestPostgresSubLive(unittest.TestCase):
    def test_sub_catch_up_then_live_inserts(self):
        """Seeded rows arrive via catch-up; mid-run inserts arrive via NOTIFY."""
        import threading
        import time as time_mod

        import psycopg
        from wingfoil import postgres_notify_trigger_sql, postgres_sub

        _reset_table(
            [("2000-01-01 00:00:00", "SEED0", 1.0, 10)]
        )
        with psycopg.connect(CONN_STR, autocommit=True) as conn:
            conn.execute(postgres_notify_trigger_sql("py_pg_trades", "py_pg_feed"))

        def insert_later():
            time_mod.sleep(0.5)
            with psycopg.connect(CONN_STR, autocommit=True) as conn:
                conn.execute(
                    "INSERT INTO py_pg_trades VALUES "
                    "('2000-01-01 00:00:01', 'LIVE1', 2.0, 20)"
                )
                conn.execute(
                    "INSERT INTO py_pg_trades VALUES "
                    "('2000-01-01 00:00:02', 'LIVE2', 3.0, 30)"
                )

        inserter = threading.Thread(target=insert_later)
        inserter.start()

        stream = postgres_sub(
            CONN_STR,
            "SELECT time, sym, price, qty FROM py_pg_trades",
            "time",
            "py_pg_feed",
            start=0.0,  # cursor in the past -> catch-up picks up SEED0
        ).collect()
        stream.run(realtime=True, duration=3.0)
        inserter.join()

        rows = stream.peek_value()
        self.assertEqual([r["sym"] for r in rows], ["SEED0", "LIVE1", "LIVE2"])
