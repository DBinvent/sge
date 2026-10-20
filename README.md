# Schema Guard Enterprise \[sge\]
The relational database schema management tool.

> [!NOTE]
The Schema Guard as YAML library available in schema_guard_tokio crate or https://github.com/vkrinitsyn/schema_guard/tree/std


## SQL script tracker and DB deployment tool by schema definition for Postgres

Combines two ways:

1. Declarative for rapid development,
    with schema definition, comparison and versioning
    
2. Imperative for full review and control,
    with script execution tracking, filtered by version and tag


Also, suitable as orchestrator for multiple db instances, hosts and environments.
It is microservice-ready: several databases (one per microservice) and several environments.


#### Key features

 - create database if absent
 - ability to connect by certs
 - git repository as a script source, with the deployed revision recorded
 - YT Cluster aware: a cluster's own schema migration options (node-by-node rolling is planned)
 - use both: schema/table definition and script execution tracking
 - simple ETL for extraction on snapshot and load data on migration
 - support separate db connection for history table as a remote copy or primary one
 - use psql client for script execution, include native psql script support
 - no JVM nor Python required to run
 - lightweight (just about 10MB)
 - full replacement of flyway
 - transactional changes with rollback OR use configurable non-transactional mode
 - dry run and diff with existing DB
 - no license key, no activation, no expiry


#### Limitations

 - Postgres DB only, psql client required
 - see 'Schema deploy restriction notice' below


### Usage

Options go on the command line (`--db_name=app`) or, by the same names without
the `--`, into a config file given with `-c`: `key=value` lines, `#` for comments.

```
sge -c sge.cfg migrate                         # apply what source_dir holds to db_name, recorded in sge_history
sge -c sge.cfg --dry_run=yes migrate           # the same, without changing the database
sge -c sge.cfg prepare                         # create the history table only
sge -c sge.cfg --snapshot_to=./schema snapshot # dump the schema as YAML
sge -c sge.cfg info                            # the deployment status, as YAML
sge help migrate                               # every option of a command
```

```
# sge.cfg
db_host=localhost
db_name=app
db_user=app
source_dir=./sql
```

Run in a terminal, sge first prints its banner; piped or redirected, it prints
only its output.

### Schema files

S files (`S<version>__<name>.yaml`) are migrated by
[schema_guard](https://github.com/vkrinitsyn/schema_guard) (`schema_guard_tokio`),
with the same parse and the same migration as its `migrate()`: each file's
tables, then their foreign keys. Templates come with it: a `template: true`
table is never created, and a `template: [name]` table carries the template's
columns, triggers and grants before its own. sge adds the versions, the history
and the ETL around that, and a commit after each changed table with
`schema_transaction=table`, `retry`, or a table's own `transaction: table`.

### Schema/Table deploy by definition restriction notice

 - Schemas and tables definition allowed only in YAML format 
 - Adding sql & constraint on existing column/tables won't produce an ALTER TABLE SQL (except FK but sql)
 - The composite primary key (multi columns) only on create table 
 

### Schema snapshot restriction notice

 - Only column name and type, pk, will be created, but not null and fk constraint 
 - No indexes, constraint, special sql on table or column definition

### Packages

The packages are named after the crate, `schema_guard` - the deb `schema-guard`,
as Debian names take no `_` - and install the `sge` binary. From the first release
under that name (they were `rdbm` before) they are published in DBinvent's
repository at apt.dbinvent.com. Until then, build it from source (below).

### Build from source

```
cargo build --release      # target/release/sge
cargo test --lib --bins    # the unit tests, no database needed
```

The integration tests (`tests/`) start a throwaway PostgreSQL of their own with
`tests/postgres.sh` - its `initdb`, found under `/usr`, on a free port in
`/tmp/tests` - so they need the PostgreSQL server binaries installed. `PG_VER`
picks the version (default: the newest installed); `SGE_IT=ignore` skips them,
and without it a database that cannot be prepared fails the test. See
`tests/README.md`.

### Checks

```
cargo audit      # known vulnerabilities (RustSec) in the dependencies Cargo.lock pins
cargo geiger     # unsafe code in sge and in each dependency it uses
```

`cargo install cargo-audit cargo-geiger` installs them; with cargo-make,
`cargo make audit` and `cargo make geiger` run the same. geiger reads every
dependency file in the target directory, so give it one of its own -
`CARGO_TARGET_DIR=target/geiger cargo geiger` - or an old build's can name a
source file that is gone.

### Git as a script source

Point sge at a repository instead of a directory and it clones into a local
cache, checks out the requested revision, and reads the scripts from there.
Everything downstream - filters, version parsing, checksums, history - is
unchanged, so a git source cannot alter which scripts run or how they are
recorded.

| key | meaning |
|---|---|
| `source_git` | repository URL. Empty (the default) keeps the plain directory behaviour. |
| `source_git_ref` | branch, tag or commit. Default: the remote's HEAD. |
| `source_git_dir` | sub-directory inside the repo holding the scripts. |
| `source_git_cache` | where clones live. Default `<tmp>/sge-git`. |
| `source_git_depth` | shallow depth, default `1`. `0` fetches full history. |

sge shells out to the `git` binary for the same reason it shells out to
`psql`: the operator's existing credentials just work - ssh-agent, credential
helpers, `~/.gitconfig`, `insteadOf` rewrites, proxies and CA bundles - with no
second authentication model to configure.

The resolved commit is written to `sge_history.script_tag` when no explicit
`script_tag` is set, so the history answers *which revision produced this
database* - the question a directory path cannot.

### YT Cluster

With `yt_cluster = true`, sge reads the cluster's own SchemaGuard options - the
`C sg_migration` row of its config table - and applies YAML schema files by
them: the same options the cluster judges a schema difference by.

Rolling a migration across the cluster - one node at a time over `psql`,
**followers first, leader last**, **halting at the first failure** - is planned:
the driver is in `src/dbm_yt.rs`, and the SQL planner it needs is not in this
release.

| key | meaning |
|---|---|
| `yt_cluster` | enable rolling migration. Default `false`. |
| `yt_config_table` | the cluster config table. Default `public.yt_config`. |

Only the *host* is taken from the cluster roster: module `N` carries the
cluster's own RPC endpoint (`host:7421`), which is not a PostgreSQL port. User,
port, password and TLS come from the ordinary `db_*` configuration.

The planner will be SQL and the driver sge because PostgreSQL cannot reach a
peer without `dblink`/`postgres_fdw`, and requiring those would mean storing
credentials for every node on every node. Serial application is what keeps the
cluster serving - every node not currently migrating is up - and what makes a
failure survivable: the roll stops with a *known* number of nodes migrated, and
the history table says exactly which.

> #### ⚠ Migrations rolled this way must be backward compatible
>
> **A rolling migration means the cluster runs a mixed schema for the duration
> of the roll.** While node 3 of 6 is migrating, three nodes have the new schema
> and three have the old one, and application traffic is being served by both.
> There is no window in which this is not true, and shortening the window does
> not make it safe.
>
> Every migration rolled across a cluster must therefore satisfy one of:
>
> 1. **The change is backward compatible** - old code runs unchanged against the
>    new schema: adding a nullable column, adding a table or index, widening a
>    type, adding an optional parameter. This should be the overwhelming
>    majority of migrations.
>
> 2. **The code is schema-adaptive** - it detects which shape it is talking to
>    and handles both. This is required when a change genuinely cannot be made
>    compatible, and the adaptive code must be deployed **before** the
>    migration, never with it.
>
> A change that is neither - dropping a column still read, renaming in place,
> adding `NOT NULL` without a default, narrowing a type - **will break the nodes
> that have not migrated yet, and equally the ones that have.**
>
> sge cannot detect this for you, because whether a column is still read is a
> fact about application code, not about the database. Use expand/contract:
> expand (compatible), deploy code, contract (compatible) - three rolls, each
> individually safe.

### Upgrading from rdbm

sge is rdbm renamed - the binary (`sge`), the package (`schema-guard`, see
Packages), and the history table: its default is now `sge_history`, so a database
rdbm migrated looks unmigrated to sge. Point sge at the old table with
`--table=rdbm_history`, or rename the table, before the first run. Configuration
keys are unchanged.

### License

MIT, as Schema Guard is - see `LICENSE`. Copyright (c) 2020-2026 DBinvent.

There is no license key, signature check, activation or expiry.

