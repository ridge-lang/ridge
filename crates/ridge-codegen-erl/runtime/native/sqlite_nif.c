/* sqlite_nif — the native bridge that backs the std.data SQLite adapter.
 *
 * SQLite is an embedded C library with no network protocol, so unlike the
 * pure-Erlang Postgres client it cannot be reached over a socket: the only way
 * onto the BEAM is a native function. This file is that bridge and nothing
 * more. It is deliberately small and does no type interpretation of its own —
 * ridge_sqlite.erl owns the SqlValue mapping. Everything here is either a
 * checked sqlite3_* call or a straight translation between an Erlang term and a
 * bound parameter / result cell, so the memory-unsafe surface stays auditable.
 *
 * Every exported function runs on a dirty I/O scheduler
 * (ERL_NIF_DIRTY_JOB_IO_BOUND): a SQLite call can block on disk for as long as
 * a query takes, and a blocking call on a normal scheduler would stall the
 * whole node. The amalgamation is compiled with SQLITE_THREADSAFE=1 and each
 * connection is opened SQLITE_OPEN_FULLMUTEX (serialized mode), so concurrent
 * calls on one connection are memory-safe; ordering across callers is the
 * adapter's concern, mirroring how the Postgres client serialises a socket.
 *
 * Wire contract with ridge_sqlite.erl:
 *   - a connection is an opaque resource term
 *   - a bound parameter is one of {int,N} | {float,F} | {text,B} | {blob,B}
 *     | the atom null
 *   - a result cell is the same vocabulary, chosen by SQLite storage class
 *   - errors are {error, {sqlite_error, Code, Msg}} (a checked sqlite3 return)
 *     or {error, {bad_param, Term}} (a term that is not a valid cell)
 */

#include <string.h>
#include "erl_nif.h"
#include "sqlite3.h"

/* How long a busy connection waits for a lock before returning SQLITE_BUSY.
 * Matches the order of magnitude of the Postgres client's checkout timeout. */
#define RIDGE_SQLITE_BUSY_TIMEOUT_MS 5000

static ErlNifResourceType *conn_res_type;

static ERL_NIF_TERM atom_ok;
static ERL_NIF_TERM atom_error;
static ERL_NIF_TERM atom_null;
static ERL_NIF_TERM atom_int;
static ERL_NIF_TERM atom_float;
static ERL_NIF_TERM atom_text;
static ERL_NIF_TERM atom_blob;
static ERL_NIF_TERM atom_sqlite_error;
static ERL_NIF_TERM atom_bad_param;
static ERL_NIF_TERM atom_closed;

/* A connection resource. `closed` guards against use or free after an explicit
 * close: the destructor and nif_close both flip it, and every entry point
 * checks it, so a double close or a call on a closed handle fails cleanly
 * instead of touching a freed sqlite3*. */
typedef struct {
    sqlite3 *db;
    int closed;
} conn_t;

static void conn_dtor(ErlNifEnv *env, void *obj) {
    conn_t *c = (conn_t *)obj;
    (void)env;
    if (c->db != NULL && !c->closed) {
        /* close_v2 defers the actual close if statements are still open, so it
         * never fails hard even if a caller dropped the handle mid-query. */
        sqlite3_close_v2(c->db);
        c->db = NULL;
        c->closed = 1;
    }
}

static ERL_NIF_TERM make_binary(ErlNifEnv *env, const void *data, size_t len) {
    ERL_NIF_TERM term;
    unsigned char *buf = enif_make_new_binary(env, len, &term);
    if (len > 0) {
        memcpy(buf, data, len);
    }
    return term;
}

/* {error, {sqlite_error, Code, Msg}} from the last error on `db` (or the
 * generic string for `rc` when no connection is available yet). */
static ERL_NIF_TERM make_sqlite_error(ErlNifEnv *env, sqlite3 *db, int rc) {
    const char *msg = (db != NULL) ? sqlite3_errmsg(db) : sqlite3_errstr(rc);
    ERL_NIF_TERM msg_term = make_binary(env, msg, strlen(msg));
    ERL_NIF_TERM inner =
        enif_make_tuple3(env, atom_sqlite_error, enif_make_int(env, rc), msg_term);
    return enif_make_tuple2(env, atom_error, inner);
}

static ERL_NIF_TERM make_bad_param(ErlNifEnv *env, ERL_NIF_TERM term) {
    return enif_make_tuple2(env, atom_error,
                            enif_make_tuple2(env, atom_bad_param, term));
}

/* Fetch and validate the connection resource, rejecting a closed handle. */
static int get_conn(ErlNifEnv *env, ERL_NIF_TERM term, conn_t **out) {
    conn_t *c;
    if (!enif_get_resource(env, term, conn_res_type, (void **)&c)) {
        return 0;
    }
    if (c->closed || c->db == NULL) {
        return 0;
    }
    *out = c;
    return 1;
}

/* Bind one positional parameter (1-based `idx`) from a cell term. Returns 1 on
 * success; on failure returns 0 and writes the error term to *err. */
static int bind_one(ErlNifEnv *env, sqlite3 *db, sqlite3_stmt *stmt, int idx,
                    ERL_NIF_TERM term, ERL_NIF_TERM *err) {
    int rc;
    int arity;
    const ERL_NIF_TERM *elems;
    ERL_NIF_TERM tag;

    if (enif_is_identical(term, atom_null)) {
        rc = sqlite3_bind_null(stmt, idx);
        if (rc != SQLITE_OK) {
            *err = make_sqlite_error(env, db, rc);
            return 0;
        }
        return 1;
    }

    if (!enif_get_tuple(env, term, &arity, &elems) || arity != 2) {
        *err = make_bad_param(env, term);
        return 0;
    }
    tag = elems[0];

    if (enif_is_identical(tag, atom_int)) {
        ErlNifSInt64 v;
        if (!enif_get_int64(env, elems[1], &v)) {
            *err = make_bad_param(env, term);
            return 0;
        }
        rc = sqlite3_bind_int64(stmt, idx, v);
    } else if (enif_is_identical(tag, atom_float)) {
        double v;
        if (!enif_get_double(env, elems[1], &v)) {
            *err = make_bad_param(env, term);
            return 0;
        }
        rc = sqlite3_bind_double(stmt, idx, v);
    } else if (enif_is_identical(tag, atom_text)) {
        ErlNifBinary b;
        if (!enif_inspect_binary(env, elems[1], &b)) {
            *err = make_bad_param(env, term);
            return 0;
        }
        /* SQLITE_TRANSIENT: sqlite copies the bytes now, so the Erlang binary
         * is free to move or be collected after this call. */
        rc = sqlite3_bind_text(stmt, idx, (const char *)b.data, (int)b.size,
                               SQLITE_TRANSIENT);
    } else if (enif_is_identical(tag, atom_blob)) {
        ErlNifBinary b;
        if (!enif_inspect_binary(env, elems[1], &b)) {
            *err = make_bad_param(env, term);
            return 0;
        }
        rc = sqlite3_bind_blob(stmt, idx, b.data, (int)b.size, SQLITE_TRANSIENT);
    } else {
        *err = make_bad_param(env, term);
        return 0;
    }

    if (rc != SQLITE_OK) {
        *err = make_sqlite_error(env, db, rc);
        return 0;
    }
    return 1;
}

/* Walk the parameter list, binding each element by position. On any failure the
 * statement is left for the caller to finalize. */
static int bind_params(ErlNifEnv *env, sqlite3 *db, sqlite3_stmt *stmt,
                       ERL_NIF_TERM params, ERL_NIF_TERM *err) {
    ERL_NIF_TERM head;
    ERL_NIF_TERM tail = params;
    int idx = 1;

    if (!enif_is_list(env, params)) {
        *err = make_bad_param(env, params);
        return 0;
    }
    while (enif_get_list_cell(env, tail, &head, &tail)) {
        if (!bind_one(env, db, stmt, idx, head, err)) {
            return 0;
        }
        idx++;
    }
    return 1;
}

/* One result cell, chosen by SQLite's per-value storage class. */
static ERL_NIF_TERM decode_cell(ErlNifEnv *env, sqlite3_stmt *stmt, int col) {
    switch (sqlite3_column_type(stmt, col)) {
        case SQLITE_INTEGER:
            return enif_make_tuple2(
                env, atom_int,
                enif_make_int64(env, sqlite3_column_int64(stmt, col)));
        case SQLITE_FLOAT:
            return enif_make_tuple2(
                env, atom_float,
                enif_make_double(env, sqlite3_column_double(stmt, col)));
        case SQLITE_TEXT: {
            const void *p = sqlite3_column_text(stmt, col);
            int n = sqlite3_column_bytes(stmt, col);
            return enif_make_tuple2(env, atom_text, make_binary(env, p, (size_t)n));
        }
        case SQLITE_BLOB: {
            const void *p = sqlite3_column_blob(stmt, col);
            int n = sqlite3_column_bytes(stmt, col);
            return enif_make_tuple2(env, atom_blob, make_binary(env, p, (size_t)n));
        }
        case SQLITE_NULL:
        default:
            return atom_null;
    }
}

/* nif_open(PathBin) -> {ok, Conn} | {error, {sqlite_error, Code, Msg}}
 * Opens (creating if absent) a serialized read/write connection. ":memory:"
 * opens a private in-memory database. */
static ERL_NIF_TERM open_nif(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[]) {
    ErlNifBinary path;
    char *cpath;
    sqlite3 *db = NULL;
    int rc;
    conn_t *c;
    ERL_NIF_TERM term;
    (void)argc;

    if (!enif_inspect_binary(env, argv[0], &path)) {
        return enif_make_badarg(env);
    }

    /* sqlite3_open_v2 wants a NUL-terminated path; an Erlang binary is not. */
    cpath = (char *)enif_alloc(path.size + 1);
    if (cpath == NULL) {
        return enif_make_badarg(env);
    }
    memcpy(cpath, path.data, path.size);
    cpath[path.size] = '\0';

    rc = sqlite3_open_v2(cpath, &db,
                         SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE |
                             SQLITE_OPEN_FULLMUTEX,
                         NULL);
    enif_free(cpath);

    if (rc != SQLITE_OK) {
        ERL_NIF_TERM err = make_sqlite_error(env, db, rc);
        sqlite3_close_v2(db); /* v2 tolerates a NULL or half-open handle */
        return err;
    }

    sqlite3_busy_timeout(db, RIDGE_SQLITE_BUSY_TIMEOUT_MS);

    c = (conn_t *)enif_alloc_resource(conn_res_type, sizeof(conn_t));
    if (c == NULL) {
        sqlite3_close_v2(db);
        return enif_make_badarg(env);
    }
    c->db = db;
    c->closed = 0;

    term = enif_make_resource(env, c);
    enif_release_resource(c); /* the term now owns the only reference */
    return enif_make_tuple2(env, atom_ok, term);
}

/* nif_close(Conn) -> ok | {error, closed}
 * Idempotent from the caller's view: a second close reports {error, closed}
 * rather than touching a freed handle. */
static ERL_NIF_TERM close_nif(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[]) {
    conn_t *c;
    (void)argc;

    if (!enif_get_resource(env, argv[0], conn_res_type, (void **)&c)) {
        return enif_make_badarg(env);
    }
    if (c->closed || c->db == NULL) {
        return enif_make_tuple2(env, atom_error, atom_closed);
    }
    sqlite3_close_v2(c->db);
    c->db = NULL;
    c->closed = 1;
    return atom_ok;
}

/* nif_exec(Conn, SqlBin, Params) -> {ok, AffectedRows} | {error, _}
 * Runs one statement that returns no rows (INSERT/UPDATE/DELETE/DDL/BEGIN/...),
 * discarding any rows it happens to produce, and answers sqlite3_changes. */
static ERL_NIF_TERM exec_nif(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[]) {
    conn_t *c;
    ErlNifBinary sql;
    sqlite3_stmt *stmt = NULL;
    ERL_NIF_TERM err;
    int rc;
    (void)argc;

    if (!get_conn(env, argv[0], &c)) {
        return enif_make_tuple2(env, atom_error, atom_closed);
    }
    if (!enif_inspect_binary(env, argv[1], &sql)) {
        return enif_make_badarg(env);
    }

    rc = sqlite3_prepare_v2(c->db, (const char *)sql.data, (int)sql.size, &stmt, NULL);
    if (rc != SQLITE_OK) {
        return make_sqlite_error(env, c->db, rc);
    }
    if (!bind_params(env, c->db, stmt, argv[2], &err)) {
        sqlite3_finalize(stmt);
        return err;
    }

    do {
        rc = sqlite3_step(stmt);
    } while (rc == SQLITE_ROW);

    if (rc != SQLITE_DONE) {
        err = make_sqlite_error(env, c->db, rc);
        sqlite3_finalize(stmt);
        return err;
    }
    sqlite3_finalize(stmt);
    return enif_make_tuple2(env, atom_ok,
                            enif_make_int(env, sqlite3_changes(c->db)));
}

/* nif_query(Conn, SqlBin, Params) -> {ok, Cols, Rows} | {error, _}
 * Cols is a list of column-name binaries; Rows is a list of rows, each a list
 * of cells in column order. */
static ERL_NIF_TERM query_nif(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[]) {
    conn_t *c;
    ErlNifBinary sql;
    sqlite3_stmt *stmt = NULL;
    ERL_NIF_TERM err;
    int rc;
    int ncol;
    int i;
    ERL_NIF_TERM *colnames;
    ERL_NIF_TERM cols_term;
    ERL_NIF_TERM *rows = NULL;
    size_t nrows = 0;
    size_t cap = 0;
    ERL_NIF_TERM rows_term;
    (void)argc;

    if (!get_conn(env, argv[0], &c)) {
        return enif_make_tuple2(env, atom_error, atom_closed);
    }
    if (!enif_inspect_binary(env, argv[1], &sql)) {
        return enif_make_badarg(env);
    }

    rc = sqlite3_prepare_v2(c->db, (const char *)sql.data, (int)sql.size, &stmt, NULL);
    if (rc != SQLITE_OK) {
        return make_sqlite_error(env, c->db, rc);
    }
    if (!bind_params(env, c->db, stmt, argv[2], &err)) {
        sqlite3_finalize(stmt);
        return err;
    }

    ncol = sqlite3_column_count(stmt);
    colnames = (ERL_NIF_TERM *)enif_alloc(sizeof(ERL_NIF_TERM) * (ncol > 0 ? ncol : 1));
    if (colnames == NULL) {
        sqlite3_finalize(stmt);
        return enif_make_badarg(env);
    }
    for (i = 0; i < ncol; i++) {
        const char *name = sqlite3_column_name(stmt, i);
        colnames[i] = make_binary(env, name, name ? strlen(name) : 0);
    }
    cols_term = enif_make_list_from_array(env, colnames, ncol);
    enif_free(colnames);

    while ((rc = sqlite3_step(stmt)) == SQLITE_ROW) {
        ERL_NIF_TERM *cells;
        ERL_NIF_TERM row_term;

        cells = (ERL_NIF_TERM *)enif_alloc(sizeof(ERL_NIF_TERM) * (ncol > 0 ? ncol : 1));
        if (cells == NULL) {
            if (rows != NULL) {
                enif_free(rows);
            }
            sqlite3_finalize(stmt);
            return enif_make_badarg(env);
        }
        for (i = 0; i < ncol; i++) {
            cells[i] = decode_cell(env, stmt, i);
        }
        row_term = enif_make_list_from_array(env, cells, ncol);
        enif_free(cells);

        if (nrows == cap) {
            size_t newcap = (cap == 0) ? 16 : cap * 2;
            ERL_NIF_TERM *grown =
                (ERL_NIF_TERM *)enif_realloc(rows, sizeof(ERL_NIF_TERM) * newcap);
            if (grown == NULL) {
                if (rows != NULL) {
                    enif_free(rows);
                }
                sqlite3_finalize(stmt);
                return enif_make_badarg(env);
            }
            rows = grown;
            cap = newcap;
        }
        rows[nrows++] = row_term;
    }

    if (rc != SQLITE_DONE) {
        err = make_sqlite_error(env, c->db, rc);
        if (rows != NULL) {
            enif_free(rows);
        }
        sqlite3_finalize(stmt);
        return err;
    }

    rows_term = enif_make_list_from_array(env, rows, (unsigned)nrows);
    if (rows != NULL) {
        enif_free(rows);
    }
    sqlite3_finalize(stmt);
    return enif_make_tuple3(env, atom_ok, cols_term, rows_term);
}

/* nif_libversion() -> Binary. The runtime version of the linked amalgamation,
 * so ridge_sqlite.erl can assert it matches the vendored pin at load time. */
static ERL_NIF_TERM libversion_nif(ErlNifEnv *env, int argc, const ERL_NIF_TERM argv[]) {
    const char *v = sqlite3_libversion();
    (void)argc;
    (void)argv;
    return make_binary(env, v, strlen(v));
}

static int load(ErlNifEnv *env, void **priv_data, ERL_NIF_TERM load_info) {
    (void)priv_data;
    (void)load_info;
    conn_res_type = enif_open_resource_type(env, NULL, "ridge_sqlite_conn",
                                            conn_dtor, ERL_NIF_RT_CREATE, NULL);
    if (conn_res_type == NULL) {
        return -1;
    }
    atom_ok = enif_make_atom(env, "ok");
    atom_error = enif_make_atom(env, "error");
    atom_null = enif_make_atom(env, "null");
    atom_int = enif_make_atom(env, "int");
    atom_float = enif_make_atom(env, "float");
    atom_text = enif_make_atom(env, "text");
    atom_blob = enif_make_atom(env, "blob");
    atom_sqlite_error = enif_make_atom(env, "sqlite_error");
    atom_bad_param = enif_make_atom(env, "bad_param");
    atom_closed = enif_make_atom(env, "closed");
    return 0;
}

static ErlNifFunc nif_funcs[] = {
    {"nif_open", 1, open_nif, ERL_NIF_DIRTY_JOB_IO_BOUND},
    {"nif_close", 1, close_nif, ERL_NIF_DIRTY_JOB_IO_BOUND},
    {"nif_exec", 3, exec_nif, ERL_NIF_DIRTY_JOB_IO_BOUND},
    {"nif_query", 3, query_nif, ERL_NIF_DIRTY_JOB_IO_BOUND},
    {"nif_libversion", 0, libversion_nif, 0},
};

ERL_NIF_INIT(ridge_sqlite, nif_funcs, load, NULL, NULL, NULL)
