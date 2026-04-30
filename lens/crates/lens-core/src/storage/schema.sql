CREATE TABLE meta (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    schema_version INTEGER NOT NULL,
    lens_version TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%s','now')),
    indexed_at INTEGER
) STRICT;

CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    language TEXT NOT NULL,
    content_hash BLOB NOT NULL,
    size_bytes INTEGER NOT NULL,
    modified_at INTEGER NOT NULL,
    indexed_at INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_files_language ON files(language);
CREATE INDEX idx_files_content_hash ON files(content_hash);

CREATE TABLE symbols (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    qualified_name TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    start_col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    body_start_byte INTEGER NOT NULL,
    body_end_byte INTEGER NOT NULL,
    signature TEXT,
    visibility TEXT,
    parent_symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE
    -- v2 adds `doc_comment TEXT` via ALTER TABLE in migrations.rs; the
    -- column is NOT in this v1 schema definition because the v2 migration
    -- ALTER TABLE would conflict.
) STRICT;

CREATE UNIQUE INDEX idx_symbols_qname_file ON symbols(file_id, qualified_name);
CREATE INDEX idx_symbols_name ON symbols(name);
CREATE INDEX idx_symbols_kind ON symbols(kind);
CREATE INDEX idx_symbols_qualified_name ON symbols(qualified_name);
CREATE INDEX idx_symbols_parent ON symbols(parent_symbol_id);

CREATE TABLE refs (
    id INTEGER PRIMARY KEY,
    symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    end_col INTEGER NOT NULL,
    kind TEXT NOT NULL,
    raw_name TEXT NOT NULL
) STRICT;

CREATE INDEX idx_refs_symbol ON refs(symbol_id);
CREATE INDEX idx_refs_file ON refs(file_id);
CREATE INDEX idx_refs_raw_name ON refs(raw_name);

CREATE TABLE calls (
    id INTEGER PRIMARY KEY,
    caller_symbol_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    callee_symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
    callee_raw_name TEXT NOT NULL,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    line INTEGER NOT NULL,
    col INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_calls_caller ON calls(caller_symbol_id);
CREATE INDEX idx_calls_callee ON calls(callee_symbol_id);
CREATE INDEX idx_calls_callee_name ON calls(callee_raw_name);

CREATE TABLE imports (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    raw_path TEXT NOT NULL,
    resolved_file_id INTEGER REFERENCES files(id) ON DELETE SET NULL,
    resolved_symbol_id INTEGER REFERENCES symbols(id) ON DELETE SET NULL,
    alias TEXT,
    line INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_imports_file ON imports(file_id);
CREATE INDEX idx_imports_raw_path ON imports(raw_path);

CREATE TABLE types (
    id INTEGER PRIMARY KEY,
    symbol_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    relation TEXT NOT NULL,
    target_symbol_id INTEGER REFERENCES symbols(id) ON DELETE CASCADE,
    target_raw_name TEXT NOT NULL,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    line INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_types_symbol ON types(symbol_id);
CREATE INDEX idx_types_target ON types(target_symbol_id);
