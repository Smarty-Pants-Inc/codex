-- Presence means ON. A new or forked thread has no row and starts OFF.
CREATE TABLE thread_keep_working (
    thread_id TEXT PRIMARY KEY NOT NULL
);
