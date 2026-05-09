# ADR-0005: DB strategy - disk WAL with SD-card-tuned PRAGMAs

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

The BBS deployment target is a Raspberry Pi with an SD card as
storage. This shapes the DB strategy more than any other constraint.

SD cards have specific behaviours we have to design around:

- **Wear amplification.** Repeated writes to the same logical
  blocks shorten card life. Workloads dominated by small frequent
  writes are particularly bad.
- **fsync stalls.** SD cards have unpredictable write latency due
  to internal garbage collection. An `fsync()` call can return
  in 1ms or 5 seconds depending on the card's mood.
- **Power-loss tolerance is poor.** Many consumer SD cards corrupt
  on power loss during writes. Some (industrial-grade) handle it
  fine. Operators choose their card.
- **Limited write throughput.** Class 10 / U3 cards advertise 30+
  MB/s sustained but often deliver much less for small random
  writes. We can't assume bandwidth headroom.

Mesh-citadel's approach was: hold the entire DB in RAM
(`:memory:` SQLite), periodically `backup()` to a disk file. This
avoided continuous SD writes but introduced a different failure:
when the periodic backup got stuck (May 8 incident), it wedged
the entire DB worker thread because aiosqlite serialises through
a single thread per connection. The BBS hung; only `kill -9`
recovered it. Even after our timeout fix, the underlying pattern
is fragile.

## Decision

**Disk-only SQLite in WAL mode**, with PRAGMA tuning chosen for SD
cards. No in-memory + backup pattern. We use `sqlx` with the
`sqlite` feature for compile-time-checked queries.

### Connection pooling

- **Read pool**: `cpu_count + 2` connections. WAL mode lets readers
  run concurrently without blocking the writer.
- **Write pool**: 1 connection. SQLite is single-writer at the file
  level; multiple write connections just contend on the file lock.

This isolates failures: a stuck operation on one connection blocks
that connection only. The mesh-citadel "one wedge wedges everything"
class is structurally impossible.

### PRAGMA settings

Applied at connection time on every connection in both pools:

```sql
PRAGMA journal_mode      = WAL;
PRAGMA synchronous       = NORMAL;
PRAGMA cache_size        = -8000;       -- 8 MB page cache
PRAGMA mmap_size         = 268435456;   -- 256 MB memory-mapped reads
PRAGMA temp_store        = MEMORY;
PRAGMA wal_autocheckpoint = 10000;      -- ~40 MB between checkpoints
PRAGMA journal_size_limit = 67108864;   -- cap WAL at 64 MB
PRAGMA foreign_keys      = ON;
PRAGMA busy_timeout      = 5000;
```

Each setting picked for a specific reason:

- **`journal_mode = WAL`**: readers don't block writers; writers
  don't block readers. Standard for any non-trivial SQLite workload.
- **`synchronous = NORMAL`**: fsync only on WAL checkpoint, not on
  every commit. This is the SD-card-friendly choice. Worst-case
  power-loss data loss is the last few hundred ms of writes; the
  database itself stays consistent. For a hobbyist BBS this trade
  is correct.
- **`cache_size = -8000`**: 8 MB of in-memory page cache. Negative
  values are KB. Enough to hold the working set of a small BBS.
- **`mmap_size = 256 MB`**: memory-mapped reads bypass the
  read syscall entirely. Reduces overhead for reports / audit-log
  scans. The cost is virtual memory address space, which a Pi has
  plenty of.
- **`temp_store = MEMORY`**: temp tables and sort buffers stay in
  RAM. Prevents temp-file thrashing on the SD card during reports.
- **`wal_autocheckpoint = 10000`**: ~40 MB of WAL between
  checkpoints. Default is 1000 (~4 MB), which on an SD card means
  a fsync stall every few seconds under load.
- **`journal_size_limit = 64 MB`**: cap on WAL file growth. Without
  this, a long-running write transaction can grow the WAL
  indefinitely.
- **`foreign_keys = ON`**: SQLite defaults this OFF, which silently
  ignores all FK constraints. We always want them on.
- **`busy_timeout = 5000`**: 5 seconds waiting for a writer lock
  before erroring. Prevents transient contention from surfacing
  as user-visible failures.

### Backups

A separate task runs `VACUUM INTO 'backup-YYYY-MM-DD-HHMMSS.sqlite'`
on a configurable interval (default: every 6 hours). This produces
a point-in-time copy on disk, distinct from the live DB. Operators
can `scp` these off the box for off-host retention.

`VACUUM INTO` is non-blocking: it runs as a separate transaction
that doesn't block the live DB. This is the structural fix for the
mesh-citadel May 8 incident - backup is no longer in the critical
path of any user request.

Old backups are pruned per the retention policy in config (default:
keep last 7 daily backups + last 4 weekly backups).

## Alternatives considered

### Keep in-memory + periodic backup (mesh-citadel pattern)

Rejected. The May 8 incident is the reference failure mode. Even
with the timeout fix we shipped, the architecture is fundamentally
"one slow write blocks everything." Going forward we want failure
isolation built into the architecture, not patched in.

### PostgreSQL

Rejected. Adds a second daemon to operate, more RAM use, more
complex backup story. The deployment scale (hobbyist, ≤a few
hundred users) doesn't warrant it. SQLite with WAL is more than
fast enough.

### Disk-only without WAL

Rejected. Default `synchronous = FULL` + DELETE journal mode
fsyncs on every transaction commit. On an SD card this is brutal
under any sustained write load, and readers block writers.

### `synchronous = OFF`

Rejected. We'd lose durability entirely on power loss - not just
the last few ms but potentially the last several seconds, plus
some risk of database corruption. Too aggressive.

### `synchronous = FULL`

Rejected. Fsync on every commit is the conservative choice but
murders SD-card lifetime. NORMAL is the right balance for our
hardware target.

## Consequences

### Positive

- **Failure isolation** at the connection level
- **Predictable disk pressure**: ~40 MB checkpoint events at a
  bounded interval rather than tiny continuous writes
- **No in-memory state to lose** on crash. Everything important
  is durable subject only to the last few hundred ms.
- **Backups are independent** of the live DB
- **Reads scale with cores** thanks to the read pool

### Negative

- **More disk I/O than the in-memory pattern** under steady-state.
  Every write hits the SD card eventually. Acceptable given the
  workload profile (a few writes per minute under typical BBS load).
- **WAL file occupies ~40-64 MB** on disk in addition to the main
  DB. Not significant on a multi-GB SD card.
- **`synchronous = NORMAL` accepts ~250 ms of data loss** in the
  worst-case power-loss scenario. Documented; operators who want
  stricter durability can override to FULL at the cost of card
  lifetime.

## Future considerations

- **WAL2 mode** (when sqlite stabilises it) may be worth adopting
  for further write-amplification reduction.
- **Litestream-style continuous replication** could provide
  near-zero-RPO backups for operators who care. Not v1.
- **Encrypted DB** (SQLCipher) is operator-side filesystem
  encryption today. If demand emerges for built-in, revisit.
