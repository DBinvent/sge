//! [plan] What migrate would change, for a review (`sge plan`): per table, the scripts
//! that change it - DDL with its columns, DML with the rows it changes. A schema file's
//! and sge's history table's from the changes schema_guard's dry run hands over, a data
//! file's from its rows, a script's read by sqlparser - no SQL read back. Each is a step;
//! the run over, the steps are taken against the database as it is, read only: a DDL's
//! IF [NOT] EXISTS and a DO block's IF are checked - one that does not hold changes
//! nothing - and the rows are counted that an UPDATE's or a DELETE's WHERE selects, and
//! an INSERT's or a data file's whose key the table has. COPY, INSERT ... SELECT and
//! psql's commands are not read. Grouped by the Jira tickets in their comments when
//! --jira_prefix names a project
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::future::Future;
use std::ops::Range;
use std::path::Path;

use schema_guard_tokio::changes::Changes;
use sqlparser::ast::{
    AlterIndexOperation, AlterTableOperation, CommentObject, ConflictTarget, Expr, FromTable, GrantObjects, Ident, Insert,
    ObjectName, ObjectNamePart, ObjectType, OnConflict, OnConflictAction, OnInsert, Query, RenameTableNameKind, SchemaName,
    SetExpr, Statement, TableFactor, TableObject, TableWithJoins, Update, UpdateTableFromKind,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;
use sqlparser::tokenizer::{Token, Tokenizer, Whitespace};
use tokio_postgres::Client;

use crate::dbm_cfg::ConfigMap;

tokio::task_local! {
    /// [digest] one run's digest, its own task's: start sets it, take takes it
    static DIGEST: RefCell<Option<Collector>>;
}

/// the object of changes that belong to no table: a schema, a function, a lone index
const OTHER: &str = "";
/// a list of columns this long at most, the rest counted
const COLUMNS: usize = 12;
/// \i in \i in ... this deep at most
const DEPTH: usize = 8;
/// [plan] keys in one count at most
pub(crate) const KEYS: usize = 1000;
/// the statements a DO block's body is read from: the first of these words on
const CHANGES: [&str; 10] = ["alter", "create", "drop", "insert", "update", "delete", "comment", "grant", "revoke", "truncate"];
/// statements that change nothing
const NO_CHANGE: [&str; 21] = ["set", "reset", "begin", "commit", "rollback", "start", "end", "savepoint", "release",
    "show", "select", "explain", "analyze", "vacuum", "lock", "listen", "notify", "discard", "checkpoint", "prepare", "deallocate"];

/// what changes came from: its type - P, S, D, V, R, ' ' an after script, B sge's
/// own history table - version and file
#[derive(Debug, Clone, PartialEq)]
struct Source {
    kind: char,
    version: String,
    file: String,
}

/// the rows a DML changes - a TRUNCATE's, every row the table has
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Dml {
    Insert,
    Update,
    Delete,
    Truncate,
    Merge,
}

/// one table's changes from one source: its DDL, and the rows inserted, updated,
/// deleted and truncated - where the database could not count them, the statements
#[derive(Debug, Default, Clone, PartialEq)]
struct Change {
    ddl: Vec<String>,
    inserts: u64,
    updates: u64,
    deletes: u64,
    truncates: u64,
    /// the statements whose rows are not counted: on a table the database has not yet, by
    /// a query it refused, a MERGE
    uncounted: BTreeMap<Dml, u64>,
}

impl Change {
    fn ddl(&mut self, what: String) {
        if !self.ddl.contains(&what) {
            self.ddl.push(what);
        }
    }

    fn rows(&mut self, dml: Dml, n: u64) {
        match dml {
            Dml::Insert => self.inserts += n,
            Dml::Update => self.updates += n,
            Dml::Delete => self.deletes += n,
            Dml::Truncate => self.truncates += n,
            Dml::Merge => {}
        }
    }

    /// statements whose rows are not counted
    fn statements(&mut self, dml: Dml, n: u64) {
        if n > 0 {
            *self.uncounted.entry(dml).or_default() += n;
        }
    }

    fn is_empty(&self) -> bool {
        self.ddl.is_empty() && self.inserts + self.updates + self.deletes + self.truncates == 0 && self.uncounted.is_empty()
    }

    fn merge(&mut self, other: Change) {
        for d in other.ddl {
            self.ddl(d);
        }
        self.inserts += other.inserts;
        self.updates += other.updates;
        self.deletes += other.deletes;
        self.truncates += other.truncates;
        for (d, n) in other.uncounted {
            self.statements(d, n);
        }
    }
}

/// [plan] what a condition asks the database: is it there
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Object {
    /// a table, a view, an index, a sequence: schema.name
    Relation(String),
    /// a table's column, trigger, constraint, policy: schema.table and its name
    Column(String, String),
    Trigger(String, String),
    Constraint(String, String),
    Policy(String, String),
    /// a function, a type: schema.name
    Function(String),
    Type(String),
    Schema(String),
    Extension(String),
    /// a table renamed: its new name, and the one it had - in what the run makes, never asked
    Renamed(String, String),
}

impl Object {
    /// a table's column, trigger, constraint, policy: the same, of another table
    fn of(&self, table: &str) -> Object {
        match self {
            Object::Column(_, x) => Object::Column(table.into(), x.clone()),
            Object::Trigger(_, x) => Object::Trigger(table.into(), x.clone()),
            Object::Constraint(_, x) => Object::Constraint(table.into(), x.clone()),
            Object::Policy(_, x) => Object::Policy(table.into(), x.clone()),
            o => o.clone(),
        }
    }

    /// the table of a column, a trigger, a constraint, a policy
    fn table(&self) -> Option<&String> {
        match self {
            Object::Column(t, _) | Object::Trigger(t, _) | Object::Constraint(t, _) | Object::Policy(t, _) => Some(t),
            _ => None,
        }
    }
}

/// [plan] the tables the run renames: each new name, and the name the database has it by
fn renames(steps: &[Step], r: &mut HashMap<String, String>) {
    for s in steps {
        match s {
            Step::Ddl { made, .. } | Step::Made(made) => for (o, _) in made {
                if let Object::Renamed(new, old) = o {
                    let first = r.get(old).cloned().unwrap_or_else(|| old.clone());
                    r.insert(new.clone(), first);
                }
            },
            Step::When { then, otherwise, .. } => {
                renames(then, r);
                renames(otherwise, r);
            }
            _ => {}
        }
    }
}

/// [plan] how the database finds a name: by its schema; one written without - under the
/// search_path - in its first schema, where a CREATE makes it, or in the first it is in
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Lookup {
    Schema,
    First(Option<String>),
    Any(Option<String>),
}

/// where a change goes: a table - OTHER the rest - or the table of an index or a
/// sequence, which the run tells once it is over: a DROP INDEX, a CREATE SEQUENCE
/// before its table
#[derive(Debug, Clone, PartialEq)]
enum Target {
    Table(String),
    Of(String),
}

/// what an object is, made or dropped
type Made = Vec<(Object, bool)>;

/// a DDL's conditions: each object there, or not, as the database finds it
type When = Vec<(Object, bool, Lookup)>;

/// what an ON CONFLICT does with a key the table has
#[derive(Debug, Clone, Copy, PartialEq)]
enum Conflict {
    Nothing,
    Update,
    /// DO UPDATE ... WHERE: some of them, not counted
    UpdateWhere,
}

/// [plan] a step of the run, taken in its order once it is over
#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// DDL, where its conditions hold - IF [NOT] EXISTS; none, always - and what it makes
    /// and drops, for the conditions after it
    Ddl { on: Target, what: String, when: When, made: Made },
    /// what the run makes besides: a schema file's tables, columns, indexes
    Made(Made),
    /// rows its text tells: an INSERT's VALUES
    Rows { table: String, dml: Dml, n: u64 },
    /// rows counted - SELECT count(*) of what a statement changes, under its search_path;
    /// none, it cannot be - on a table the database finds so
    Count { table: String, find: Lookup, dml: Dml, sql: Option<String>, path: Option<String> },
    /// rows inserted - each key once - less those whose key the table has, counted by
    /// matched, and what ON CONFLICT does with those; those a data file's clean deletes,
    /// counted; the sequences set after, when a row loads. Not counted, its statements: an
    /// INSERT's one, a data file's one a row
    Keyed {
        table: String,
        find: Lookup,
        rows: u64,
        statements: u64,
        matched: Vec<String>,
        conflict: Conflict,
        deleted: Vec<String>,
        setval: bool,
        path: Option<String>,
    },
    /// DO's IF: the statements of the branch its condition takes - both, it cannot be asked
    When { cond: String, path: Option<String>, then: Vec<Step>, otherwise: Vec<Step> },
}

/// [plan] what the plan asks the database
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Question {
    Exists(Object, Lookup),
    /// rows of a table counted: the table, its SELECT, under a search_path
    Count(String, Lookup, String, Option<String>),
    /// a DO's IF: its condition, under a search_path
    Holds(String, Option<String>),
    /// a table written without its schema, under a search_path of more than one: the
    /// place - 1 for the first - of the schema it is in
    Found(String, String),
}

/// the database's answers: a count, 1 or 0 for yes or no, a place - a question it could
/// not answer has none
type Answers = HashMap<Question, u64>;

/// the indexes and sequences of the run, and their tables
#[derive(Debug, Default)]
struct Owners {
    indexes: HashMap<String, String>,
    sequences: HashMap<String, String>,
}

impl Owners {
    /// the table of an index or a sequence: by its schema.name, else by a name alone
    /// that is one object's
    fn table(&self, object: &str) -> Option<String> {
        let all = || self.indexes.iter().chain(self.sequences.iter());
        if let Some(t) = all().find(|(o, _)| o.as_str() == object).map(|(_, t)| t.clone()) {
            return Some(t);
        }
        let name = object.rsplit('.').next().unwrap_or(object);
        let found: BTreeSet<&String> = all().filter(|(o, _)| o.rsplit('.').next() == Some(name)).map(|(_, t)| t).collect();
        if found.len() == 1 { found.into_iter().next().cloned() } else { None }
    }
}

/// a name as read - in the search_path's first schema - written without its schema, and
/// that search_path
type Bare = BTreeSet<(String, String, String)>;

#[derive(Debug, Clone)]
struct Entry {
    source: Source,
    tickets: Vec<String>,
    steps: Vec<Step>,
    /// its names written without a schema, under a search_path of more than one
    bare: Bare,
    /// schema.table => its changes, once the steps are taken; OTHER the rest
    changes: BTreeMap<String, Change>,
    /// statements sqlparser could not read
    unread: usize,
    /// the script itself not read
    error: Option<String>,
    /// an after_each script, as many times as the run's versions
    runs: usize,
}

impl Entry {
    fn has_any(&self) -> bool {
        !self.changes.is_empty() || self.unread > 0 || self.error.is_some()
    }
}

pub(crate) struct Collector {
    target: String,
    prefixes: Vec<String>,
    current: Option<(Source, Vec<String>)>,
    /// the schema file's tickets, for its data files
    schema_tickets: Vec<String>,
    owners: Owners,
    entries: Vec<Entry>,
}

impl Collector {
    /// a source's steps; SQL that comes in parts - a schema file's - is merged, a script
    /// met again - after_each - counts one more run
    fn add(&mut self, source: Source, tickets: Vec<String>, a: Analysis, error: Option<String>, merge: bool) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.source == source) {
            if !merge {
                e.runs += 1;
                return;
            }
            e.steps.extend(a.steps);
            e.bare.extend(a.bare);
            e.unread += a.unread;
            for t in tickets {
                if !e.tickets.contains(&t) {
                    e.tickets.push(t);
                }
            }
            return;
        }
        self.entries.push(Entry { source, tickets, steps: a.steps, bare: a.bare, changes: BTreeMap::new(), unread: a.unread,
            error, runs: 1 });
    }

    /// steps under the current source
    fn steps(&mut self, steps: Vec<Step>) {
        if let Some((s, t)) = self.current.clone() {
            self.add(s, t, Analysis { steps, ..Default::default() }, None, true);
        }
    }
}

/// [digest] a run in a scope of its own: its digest - the hooks below write to it -
/// is no other run's
pub(crate) async fn scope<F: Future>(f: F) -> F::Output {
    DIGEST.scope(RefCell::new(None), f).await
}

/// [plan] the plan's digest, collected from here on in this scope
pub(crate) fn start(cfg: &ConfigMap) {
    let (name, host) = (cfg.get("db_name"), cfg.get("db_host"));
    let c = Collector {
        target: if host.is_empty() { name } else { format!("{}@{}", name, host) },
        prefixes: prefixes(&cfg.get("jira_prefix")),
        current: None,
        schema_tickets: vec![],
        owners: Owners::default(),
        entries: vec![],
    };
    let _ = DIGEST.try_with(|d| *d.borrow_mut() = Some(c));
}

/// the source of the SQL that follows: a schema file, a data file, sge's history
pub(crate) fn source(kind: char, version: &str, file: &str, path: Option<&str>) {
    with(|c| {
        let tickets = if kind == 'D' {
            c.schema_tickets.clone()
        } else {
            path.and_then(|p| fs::read_to_string(p).ok())
                .map(|t| tickets(&yaml_comments(&t), &c.prefixes))
                .unwrap_or_default()
        };
        if kind == 'S' {
            c.schema_tickets = tickets.clone();
        }
        c.current = Some((Source { kind, version: version.into(), file: file.into() }, tickets));
    });
}

/// [digest] a schema_guard dry run's table and its changes, under the current source:
/// a schema file's, sge's history table's - and what they make, for the conditions of
/// the scripts after. schema_guard writes its names unquoted: Postgres has them in lower
/// case. A table with no changes is none
pub(crate) fn changes(table: Option<&str>, changes: Option<&Changes>) {
    let (table, x) = match (table, changes) {
        (Some(t), Some(x)) => (t.to_lowercase(), x),
        _ => return,
    };
    with(|c| {
        let schema = table.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_else(|| "public".into());
        let mut made: Made = vec![];
        if x.create {
            made.push((Object::Relation(table.clone()), true));
        }
        made.extend(x.columns.iter().map(|col| (Object::Column(table.clone(), col.to_lowercase()), true)));
        made.extend(x.triggers.iter().map(|t| (Object::Trigger(table.clone(), t.to_lowercase()), true)));
        for i in &x.indexes {
            let i = format!("{}.{}", schema, i.to_lowercase());
            c.owners.indexes.insert(i.clone(), table.clone());
            made.push((Object::Relation(i), true));
        }
        let mut steps: Vec<Step> = ddl_of(x).into_iter()
            .map(|what| Step::Ddl { on: Target::Table(table.clone()), what, when: vec![], made: vec![] })
            .collect();
        steps.push(Step::Made(made));
        if x.rows > 0 {
            steps.push(Step::Rows { table: table.clone(), dml: Dml::Insert, n: x.rows as u64 });
        }
        c.steps(steps);
    });
}

/// the changes as the digest's DDL: the table created with its columns, or its columns
/// added and altered; its comment, indexes, triggers, foreign keys, grants
fn ddl_of(x: &Changes) -> Vec<String> {
    let mut ddl = vec![];
    if x.create {
        ddl.push(format!("create ({})", list(x.columns.iter().cloned())));
    } else {
        let mut ops: Vec<String> = x.columns.iter().map(|n| format!("add {}", n)).collect();
        ops.extend(x.altered.iter().map(|n| format!("alter {}", n)));
        if x.primary_key {
            ops.push("primary key".into());
        }
        if x.owner {
            ops.push("owner".into());
        }
        if !ops.is_empty() {
            ddl.push(format!("alter: {}", ops.join(", ")));
        }
    }
    if x.comment {
        ddl.push("comment".into());
    }
    ddl.extend(x.indexes.iter().map(|i| format!("index {}", i)));
    ddl.extend(x.triggers.iter().map(|t| format!("trigger {}", t)));
    ddl.extend(x.foreign_keys.iter().map(|f| format!("foreign key {}", f)));
    if !x.grants.is_empty() {
        ddl.push(format!("grant {}", x.grants.join(", ")));
    }
    ddl
}

/// [plan] a data file's rows on its table - unquoted, in lower case as Postgres has it:
/// inserted - each key once - less those the table has, by their key, else by every
/// column, counted by matched; those updated, when its rule updates; those its clean
/// deletes, counted by deleted; its sequences set, when a row loads
pub(crate) fn data(table: &str, rows: u64, matched: Vec<String>, update: bool, deleted: Vec<String>, setval: bool) {
    let conflict = if update { Conflict::Update } else { Conflict::Nothing };
    with(|c| c.steps(vec![Step::Keyed { table: table.to_lowercase(), find: Lookup::Schema, rows, statements: rows, matched,
        conflict, deleted, setval, path: None }]));
}

/// a script the dry run would run, read from its file: \i and \ir from psql's
/// directory, the source directory, and from the script's own
pub(crate) fn script(kind: char, version: &str, file: &str, dir: &str) {
    with(|c| {
        let s = Source { kind, version: version.into(), file: file.into() };
        let p = path(dir, file);
        match fs::read_to_string(&p) {
            Ok(text) => {
                let here = Path::new(&p).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
                let a = Analysis::of(&text, dir, &here, &mut c.owners);
                let t = tickets(&a.comments, &c.prefixes);
                c.add(s, t, a, None, false);
            }
            Err(e) => c.add(s, vec![], Analysis::default(), Some(e.to_string()), false),
        }
    });
}

/// a file of a directory, as psql is given it
pub(crate) fn path(dir: &str, file: &str) -> String {
    if dir.is_empty() || dir.ends_with('/') || Path::new(file).is_absolute() {
        if Path::new(file).is_absolute() { file.to_string() } else { format!("{}{}", dir, file) }
    } else {
        format!("{}/{}", dir, file)
    }
}

/// [plan] the run's collector, taken from its scope: none outside a plan's
pub(crate) fn take() -> Option<Collector> {
    DIGEST.try_with(|d| d.borrow_mut().take()).ok().flatten()
}

/// [plan] the plan in md: the run's steps taken with the database's answers - read
/// only. Without a database, every condition holds and no row is counted
pub(crate) async fn finish(c: Collector, db: Option<&Client>) -> String {
    let mut qs = BTreeSet::new();
    let mut r = HashMap::new();
    for e in &c.entries {
        renames(&e.steps, &mut r);
    }
    for e in &c.entries {
        questions(&e.steps, &r, &mut qs);
        qs.extend(e.bare.iter().map(|(_, bare, path)| Question::Found(bare.clone(), path.clone())));
    }
    let answers = match db {
        Some(db) => ask(db, qs).await,
        None => Answers::new(),
    };
    plan(c, &answers)
}

fn plan(c: Collector, answers: &Answers) -> String {
    let mut entries = c.entries;
    take_steps(&mut entries, &c.owners, answers);
    render(&entries, &c.target, !c.prefixes.is_empty())
}

fn with<F: FnOnce(&mut Collector)>(f: F) {
    let _ = DIGEST.try_with(|d| {
        if let Some(c) = d.borrow_mut().as_mut() {
            f(c)
        }
    });
}

/// [plan] what the steps ask the database - both branches of an IF's; of a table the run
/// renames, by the name the database has it by too
fn questions(steps: &[Step], r: &HashMap<String, String>, qs: &mut BTreeSet<Question>) {
    for s in steps {
        match s {
            Step::Ddl { when, .. } => for (o, _, l) in when {
                qs.insert(Question::Exists(o.clone(), l.clone()));
                if let Some(old) = o.table().and_then(|t| r.get(t)) {
                    qs.insert(Question::Exists(o.of(old), l.clone()));
                }
            },
            Step::Count { table, find, sql: Some(q), path, .. } => {
                qs.insert(Question::Exists(Object::Relation(table.clone()), find.clone()));
                qs.insert(Question::Count(table.clone(), find.clone(), q.clone(), path.clone()));
            }
            Step::Keyed { table, find, matched, deleted, path, .. } => {
                qs.insert(Question::Exists(Object::Relation(table.clone()), find.clone()));
                for q in matched.iter().chain(deleted) {
                    qs.insert(Question::Count(table.clone(), find.clone(), q.clone(), path.clone()));
                }
            }
            Step::When { cond, path, then, otherwise } => {
                qs.insert(Question::Holds(cond.clone(), path.clone()));
                questions(then, r, qs);
                questions(otherwise, r, qs);
            }
            _ => {}
        }
    }
}

type Param = dyn tokio_postgres::types::ToSql + Sync;

/// [plan] the database's answers. Each question in a read-only transaction of its own,
/// rolled back: no write a condition or a WHERE would make goes through, nor a setting it
/// changes - read only turned off - outlives it; its locks go with it, none held for the
/// next; one refused, or locked out, is not answered and costs no other. A count of a
/// table the database has not is not asked
async fn ask(db: &Client, qs: BTreeSet<Question>) -> Answers {
    let mut a = Answers::new();
    for q in qs {
        let answer = match &q {
            Question::Exists(o, l) => {
                let path = match l {
                    Lookup::First(p) | Lookup::Any(p) => p.clone(),
                    Lookup::Schema => None,
                };
                let (sql, params) = exists_sql(o, l);
                let params: Vec<&Param> = params.iter().map(|p| p as &Param).collect();
                one(db, &path, &sql, &params).await.and_then(|r| r.try_get::<_, Option<bool>>(0).ok().flatten()).map(|b| b as u64)
            }
            Question::Count(table, l, sql, path) => {
                if a.get(&Question::Exists(Object::Relation(table.clone()), l.clone())) != Some(&1) {
                    continue;
                }
                one(db, path, &format!("SELECT ({})::bigint", sql), &[]).await
                    .and_then(|r| r.try_get::<_, Option<i64>>(0).ok().flatten()).map(|n| n.max(0) as u64)
            }
            // NULL, as plpgsql's IF takes it: no
            Question::Holds(cond, path) => one(db, path, &format!("SELECT ({})::boolean", cond), &[]).await
                .and_then(|r| r.try_get::<_, Option<bool>>(0).ok()).map(|b| b.unwrap_or(false) as u64),
            Question::Found(bare, path) => {
                let list = schemas(path);
                one(db, &None, "SELECT min(array_position($1::text[], n.nspname::text)) FROM pg_class c \
                    JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relname = $2 AND n.nspname = ANY ($1::text[])",
                    &[&list as &Param, bare as &Param]).await
                    .and_then(|r| r.try_get::<_, Option<i32>>(0).ok().flatten()).map(|n| n.max(0) as u64)
            }
        };
        if let Some(n) = answer {
            a.insert(q, n);
        }
    }
    a
}

/// one question, in a read-only transaction of its own under a script's search_path - the
/// database's own, it set none - rolled back
async fn one(db: &Client, path: &Option<String>, sql: &str, params: &[&Param]) -> Option<tokio_postgres::Row> {
    let mut begin = "BEGIN READ ONLY; SET LOCAL lock_timeout = '10s'; SET LOCAL statement_timeout = '60s'".to_string();
    if let Some(p) = path {
        begin.push_str(&format!("; SET LOCAL search_path TO {}", p));
    }
    let row = match db.batch_execute(&begin).await {
        Ok(()) => db.query_one(sql, params).await.ok(),
        Err(_) => None,
    };
    let _ = db.batch_execute("ROLLBACK").await;
    row
}

/// a search_path's schemas, as written: "$user" left out, unquoted in lower case
fn schemas(path: &str) -> Vec<String> {
    path.split(',').map(|s| s.trim()).filter(|s| !s.is_empty() && *s != "\"$user\"" && *s != "$user")
        .map(|s| match s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            Some(q) => q.to_string(),
            None => s.trim_matches('\'').to_lowercase(),
        })
        .collect()
}

/// is it there: the catalog asked, by schema and name as Postgres has them - a name
/// written without its schema, under the search_path
fn exists_sql(o: &Object, l: &Lookup) -> (String, Vec<String>) {
    // schema.name: the schema, the name
    let split = |n: &str| -> (String, String) {
        match n.rsplit_once('.') {
            Some((s, n)) => (s.rsplit('.').next().unwrap_or(s).to_string(), n.to_string()),
            None => ("public".into(), n.to_string()),
        }
    };
    // the schema's oid: its own, or the search_path's first - for a CREATE - or each of it
    let schema = |s: String, p: &mut Vec<String>| -> String {
        match l {
            Lookup::Schema => {
                p.push(s);
                format!("(SELECT oid FROM pg_namespace WHERE nspname = ${})", p.len())
            }
            Lookup::First(_) => "(SELECT oid FROM pg_namespace WHERE nspname = (current_schemas(false))[1])".into(),
            Lookup::Any(_) => "ANY (SELECT oid FROM pg_namespace WHERE nspname = ANY (current_schemas(false)))".into(),
        }
    };
    // a table's oid: in its schema, or the first of the search_path's it is in
    let table = |t: &str, p: &mut Vec<String>| -> String {
        let (s, n) = split(t);
        match l {
            Lookup::Any(_) => {
                p.push(n);
                format!("(SELECT c.oid FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace WHERE c.relname = ${} \
                    AND n.nspname = ANY (current_schemas(false)) ORDER BY array_position(current_schemas(false), n.nspname) LIMIT 1)",
                    p.len())
            }
            _ => {
                let ns = schema(s, p);
                p.push(n);
                format!("(SELECT oid FROM pg_class WHERE relnamespace = {} AND relname = ${})", ns, p.len())
            }
        }
    };
    let mut p = vec![];
    let sql = match o {
        Object::Relation(r) => format!("SELECT {} IS NOT NULL", table(r, &mut p)),
        Object::Column(t, x) | Object::Trigger(t, x) | Object::Constraint(t, x) | Object::Policy(t, x) => {
            let rel = table(t, &mut p);
            p.push(x.clone());
            let (from, of, name) = match o {
                Object::Column(..) => ("pg_attribute", "attrelid", "attname = ${} AND attnum > 0 AND NOT attisdropped"),
                Object::Trigger(..) => ("pg_trigger", "tgrelid", "tgname = ${}"),
                Object::Constraint(..) => ("pg_constraint", "conrelid", "conname = ${}"),
                _ => ("pg_policy", "polrelid", "polname = ${}"),
            };
            format!("SELECT EXISTS (SELECT 1 FROM {} WHERE {} = {} AND {})", from, of, rel, name.replace("${}", &format!("${}", p.len())))
        }
        Object::Function(f) | Object::Type(f) => {
            let (s, n) = split(f);
            let ns = schema(s, &mut p);
            p.push(n);
            let (from, of, name) = if matches!(o, Object::Function(_)) { ("pg_proc", "pronamespace", "proname") } else { ("pg_type", "typnamespace", "typname") };
            format!("SELECT EXISTS (SELECT 1 FROM {} WHERE {} = {} AND {} = ${})", from, of, ns, name, p.len())
        }
        Object::Schema(s) => {
            p.push(s.clone());
            "SELECT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = $1)".into()
        }
        Object::Extension(e) => {
            p.push(e.clone());
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = $1)".into()
        }
        Object::Renamed(..) => "SELECT NULL::boolean".into(),
    };
    (sql, p)
}

/// [plan] the run's steps taken, in its order, with the database's answers: DDL whose
/// conditions hold - an object as the run made or dropped it before, else as the
/// database has it - rows counted, the branch a DO's IF takes. A condition not answered
/// holds, an IF not answered takes both branches, rows not answered are not counted.
/// An index's or a sequence's change on its table, now the run's are known; the rest
/// under other; a table written without its schema, in the one the database has it in
fn take_steps(entries: &mut [Entry], owners: &Owners, answers: &Answers) {
    let mut made: HashMap<Object, bool> = HashMap::new();
    // the tables renamed so far: each new name, and the one the database has it by
    let mut alias: HashMap<String, String> = HashMap::new();
    let record = |m: Made, made: &mut HashMap<Object, bool>, alias: &mut HashMap<String, String>| {
        if let Some((new, old)) = m.iter().find_map(|(o, _)| match o {
            Object::Renamed(new, old) => Some((new.clone(), old.clone())),
            _ => None,
        }) {
            // a rename: what the run made for the table goes with it to its new name
            let first = alias.remove(&old).unwrap_or_else(|| old.clone());
            alias.insert(new.clone(), first);
            let moved: Vec<(Object, bool)> = made.iter().filter(|(k, _)| k.table() == Some(&old)).map(|(k, v)| (k.of(&new), *v)).collect();
            made.retain(|k, _| k.table() != Some(&old) && k.table() != Some(&new));
            made.extend(moved);
            made.extend(m.into_iter().filter(|(o, _)| !matches!(o, Object::Renamed(..))));
            return;
        }
        for (o, v) in m {
            if let Object::Relation(t) = &o {
                // a table made or dropped anew: what the run made for it, and a rename to its
                // name, are gone with it
                made.retain(|k, _| k.table() != Some(t));
                alias.remove(t);
            }
            made.insert(o, v);
        }
    };
    for e in entries.iter_mut() {
        let mut todo: VecDeque<Step> = std::mem::take(&mut e.steps).into();
        while let Some(s) = todo.pop_front() {
            // a table the run renamed: its count names what the database has not - not
            // counted; one the run made or dropped: what the database has is not it
            let in_db = |t: &str, l: &Lookup, made: &HashMap<Object, bool>, alias: &HashMap<String, String>| -> Option<bool> {
                if alias.contains_key(t) {
                    return None;
                }
                if made.contains_key(&Object::Relation(t.to_string())) {
                    return Some(false);
                }
                answers.get(&Question::Exists(Object::Relation(t.to_string()), l.clone())).map(|n| *n > 0)
            };
            match s {
                Step::Ddl { on, what, when, made: m } => {
                    if when.iter().all(|(o, want, l)| there(o, l, &made, &alias, owners, answers).map_or(true, |h| h == *want)) {
                        let t = match on {
                            Target::Table(t) => t,
                            Target::Of(o) => owners.table(&o).unwrap_or_default(),
                        };
                        e.changes.entry(t).or_default().ddl(what);
                        record(m, &mut made, &mut alias);
                    }
                }
                Step::Made(m) => record(m, &mut made, &mut alias),
                Step::Rows { table, dml, n } => e.changes.entry(table).or_default().rows(dml, n),
                Step::Count { table, find, dml, sql, path } => {
                    let n = sql.filter(|_| in_db(&table, &find, &made, &alias) == Some(true))
                        .and_then(|q| answers.get(&Question::Count(table.clone(), find, q, path)).copied());
                    let c = e.changes.entry(table).or_default();
                    match n {
                        Some(n) => c.rows(dml, n),
                        None => c.statements(dml, 1),
                    }
                }
                Step::Keyed { table, find, rows, statements, matched, conflict, deleted, setval, path } => {
                    let sum = |qs: &[String]| qs.iter()
                        .map(|q| answers.get(&Question::Count(table.clone(), find.clone(), q.clone(), path.clone())).copied())
                        .sum::<Option<u64>>();
                    let there = in_db(&table, &find, &made, &alias);
                    // the rows inserted and updated - none: not counted
                    let (ins, upd) = match (there, if there == Some(true) { sum(&matched) } else { None }) {
                        // a table the database has not yet: every row is new
                        (Some(false), _) => (Some(rows), Some(0)),
                        (_, Some(m)) => (Some(rows.saturating_sub(m)), match conflict {
                            Conflict::Update => Some(m.min(rows)),
                            Conflict::UpdateWhere if m > 0 => None,
                            _ => Some(0),
                        }),
                        _ => (None, if conflict == Conflict::Nothing { Some(0) } else { None }),
                    };
                    let c = e.changes.entry(table.clone()).or_default();
                    for (dml, n) in [(Dml::Insert, ins), (Dml::Update, upd)] {
                        match n {
                            Some(n) => c.rows(dml, n),
                            None => c.statements(dml, statements),
                        }
                    }
                    if !deleted.is_empty() && there != Some(false) {
                        match if there == Some(true) { sum(&deleted) } else { None } {
                            Some(d) => c.rows(Dml::Delete, d),
                            None => c.statements(Dml::Delete, 1),
                        }
                    }
                    // the sequences set after the rows: when one loads, as migrate sets them
                    if setval && (ins != Some(0) || upd != Some(0)) {
                        c.ddl("sequence setval".into());
                    }
                }
                Step::When { cond, path, then, otherwise } => {
                    let branch: Vec<Step> = match answers.get(&Question::Holds(cond, path)) {
                        Some(1) => then,
                        Some(_) => otherwise,
                        None => then.into_iter().chain(otherwise).collect(),
                    };
                    for s in branch.into_iter().rev() {
                        todo.push_front(s);
                    }
                }
            }
        }
        for (read, bare, path) in &e.bare {
            // the run made it: in the search_path's first schema, where it is read
            if made.get(&Object::Relation(read.clone())) == Some(&true) {
                continue;
            }
            let at = answers.get(&Question::Found(bare.clone(), path.clone())).copied().unwrap_or(0) as usize;
            let list = schemas(path);
            if at > 1 && at <= list.len() {
                if let Some(c) = e.changes.remove(read) {
                    e.changes.entry(format!("{}.{}", list[at - 1], bare)).or_default().merge(c);
                }
            }
        }
        e.changes.retain(|_, c| !c.is_empty());
    }
}

/// is it there: as the run made or dropped it before, else as the database has it. What
/// the run made or dropped anew has nothing the database has: a table its columns,
/// triggers, constraints, policies, indexes; a schema its tables. A table the run renamed
/// has what it had by its name before. None: not known
fn there(o: &Object, l: &Lookup, made: &HashMap<Object, bool>, alias: &HashMap<String, String>, owners: &Owners,
    answers: &Answers) -> Option<bool> {
    if let Some(m) = made.get(o) {
        return Some(*m);
    }
    let table = match o {
        Object::Relation(r) => owners.table(r).filter(|t| t != r),
        o => o.table().cloned(),
    };
    if let Some(t) = &table {
        // what the run made for it went with it: the rest is the database's, by the old name
        if let Some(old) = alias.get(t) {
            return answers.get(&Question::Exists(o.of(old), l.clone())).map(|n| *n > 0);
        }
        if made.contains_key(&Object::Relation(t.clone())) {
            return Some(false);
        }
    }
    let schema = match o {
        Object::Relation(n) | Object::Function(n) | Object::Type(n) => n.rsplit_once('.').map(|(s, _)| s.to_string()),
        _ => table.and_then(|t| t.rsplit_once('.').map(|(s, _)| s.to_string())),
    };
    if let Some(s) = schema {
        if made.contains_key(&Object::Schema(s)) {
            return Some(false);
        }
    }
    answers.get(&Question::Exists(o.clone(), l.clone())).map(|n| *n > 0)
}

/// --jira_prefix's projects: PROJ,OPS - in upper case, as Jira's keys are
fn prefixes(s: &str) -> Vec<String> {
    s.split(',').map(|p| p.trim().trim_end_matches('-').to_uppercase()).filter(|p| !p.is_empty()).collect()
}

/// the Jira tickets of the projects in a text - PROJ-123, a whole word - each once, by
/// project and number
fn tickets(text: &str, prefixes: &[String]) -> Vec<String> {
    let mut found = BTreeSet::new();
    for (pi, p) in prefixes.iter().enumerate() {
        let mut from = 0;
        while let Some(i) = text[from..].find(p.as_str()) {
            let at = from + i;
            from = at + p.len();
            if text[..at].chars().last().map_or(false, |c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let rest = &text[from..];
            if !rest.starts_with('-') {
                continue;
            }
            let digits: String = rest[1..].chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = rest[1 + digits.len()..].chars().next();
            if !digits.is_empty() && after.map_or(true, |c| !c.is_alphanumeric() && c != '_') {
                if let Ok(n) = digits.parse::<u64>() {
                    found.insert((pi, n, format!("{}-{}", p, digits)));
                }
            }
        }
    }
    found.into_iter().map(|(_, _, t)| t).collect()
}

/// SQL's comments - -- and /* */, nested - not its strings: '...', E'...', "..." and
/// $tag$...$tag$
fn sql_comments(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'-' && b.get(i + 1) == Some(&b'-') {
            let end = text[i..].find('\n').map_or(b.len(), |e| i + e);
            out.push_str(&text[i + 2..end]);
            out.push('\n');
            i = end;
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < b.len() && depth > 0 {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            out.push_str(&text[i + 2..j.saturating_sub(2).max(i + 2)]);
            out.push('\n');
            i = j;
        } else if b[i] == b'\'' || b[i] == b'"' {
            // '' and "" are the quote itself; E'\'' is one too
            let q = b[i];
            let escapes = q == b'\'' && i > 0 && (b[i - 1] == b'E' || b[i - 1] == b'e');
            let mut j = i + 1;
            while j < b.len() {
                if escapes && b[j] == b'\\' {
                    j += 2;
                } else if b[j] == q && b.get(j + 1) == Some(&q) {
                    j += 2;
                } else if b[j] == q {
                    break;
                } else {
                    j += 1;
                }
            }
            i = j + 1;
        } else if b[i] == b'$' {
            // $tag$ ... $tag$ - a $1 parameter is no tag
            let tag_end = text[i + 1..].find(|c: char| !(c.is_alphanumeric() || c == '_')).map(|e| i + 1 + e);
            match tag_end {
                Some(e) if b[e] == b'$' && !text[i + 1..e].starts_with(|c: char| c.is_ascii_digit()) => {
                    let tag = &text[i..=e];
                    i = text[e + 1..].find(tag).map_or(b.len(), |k| e + 1 + k + tag.len());
                }
                _ => i += 1,
            }
        } else {
            i += text[i..].chars().next().map_or(1, |c| c.len_utf8());
        }
    }
    out
}

/// SQL without COPY ... FROM stdin's data: rows up to \., no SQL - an apostrophe in
/// one would open a string
fn without_copy(text: &str) -> String {
    let mut out = String::new();
    let mut copy = false;
    for line in text.lines() {
        if copy {
            copy = line.trim_end() != "\\.";
            continue;
        }
        let lower = line.trim_start().to_lowercase();
        copy = lower.starts_with("copy ") && lower.contains(" from stdin");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// a YAML file's comments: from a # that starts a line or follows a blank, outside
/// quotes and block scalars (| and >)
fn yaml_comments(text: &str) -> String {
    let mut out = vec![];
    let mut block: Option<usize> = None;
    for l in text.lines() {
        let indent = l.len() - l.trim_start().len();
        if let Some(b) = block {
            if l.trim().is_empty() || indent > b {
                continue;
            }
            block = None;
        }
        let bytes = l.as_bytes();
        let mut quote = None;
        let mut comment = None;
        for i in 0..bytes.len() {
            match (quote, bytes[i]) {
                (None, b'#') if i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t' => {
                    comment = Some(i);
                    break;
                }
                (None, q @ (b'\'' | b'"')) if i == 0 || bytes[i - 1] == b' ' || bytes[i - 1] == b':' || bytes[i - 1] == b'-' => quote = Some(q),
                (Some(q), c) if c == q => quote = None,
                _ => {}
            }
        }
        let content = comment.map_or(l, |i| &l[..i]).trim_end();
        if let Some(i) = comment {
            out.push(&l[i + 1..]);
        }
        // key: |  key: >-  - |
        let last = content.rsplit(' ').next().unwrap_or("");
        if !last.is_empty() && (last.starts_with('|') || last.starts_with('>'))
            && last[1..].chars().all(|c| c == '-' || c == '+' || c.is_ascii_digit()) {
            block = Some(indent);
        }
    }
    out.join("\n")
}


/// SQL read: its steps, its names written without a schema, the statements sqlparser
/// could not read, the comments of the files \i brings in
#[derive(Debug, Default)]
struct Analysis {
    steps: Vec<Step>,
    bare: Bare,
    unread: usize,
    comments: String,
}

struct Reader<'a> {
    a: Analysis,
    owners: &'a mut Owners,
    /// a name without its schema is in this one: SET search_path's first, else public
    schema: String,
    /// SET search_path's, as written: the database is asked under it
    path: Option<String>,
    /// names written without a schema, under a search_path of more than one
    bare: RefCell<Bare>,
    /// the WITH of the statement read: a count has it too; a WITH that writes, none
    with: Option<String>,
    with_writes: bool,
    /// \i's directory - psql's, the source directory - and \ir's, the script's
    cwd: String,
    depth: usize,
}

/// an IF's parts: its condition, its THEN's statements, its ELSE's, and where it ends
struct IfParts {
    cond: Range<usize>,
    then: Range<usize>,
    otherwise: Range<usize>,
    end: usize,
}

impl Analysis {
    fn of(text: &str, cwd: &str, here: &str, owners: &mut Owners) -> Analysis {
        let mut r = Reader::new(owners, cwd);
        r.a.comments = sql_comments(&without_copy(text));
        r.read(text, here);
        r.a.bare = r.bare.take();
        r.a
    }
}

impl<'a> Reader<'a> {
    fn new(owners: &'a mut Owners, cwd: &str) -> Self {
        Reader { a: Analysis::default(), owners, schema: "public".into(), path: None, bare: RefCell::new(Bare::new()),
            with: None, with_writes: false, cwd: cwd.into(), depth: 0 }
    }

    /// psql's script: its \i files read in their place, its other \ commands left out;
    /// a \g at a line's end ends the statement, as for psql; COPY's data is no SQL
    fn read(&mut self, text: &str, here: &str) {
        let mut sql = String::new();
        let mut copy = false;
        for line in text.lines() {
            if copy {
                copy = line.trim_end() != "\\.";
                continue;
            }
            let t = line.trim_start();
            if let Some(cmd) = t.strip_prefix('\\') {
                let mut w = cmd.split_whitespace();
                let c = w.next().unwrap_or("");
                let arg = w.next().unwrap_or("").trim_matches('\'').to_string();
                let dir = match c {
                    "i" | "include" => Some(self.cwd.clone()),
                    "ir" | "include_relative" => Some(here.to_string()),
                    _ => None,
                };
                if let Some(dir) = dir {
                    // what came before it read first, then the file
                    let before = std::mem::take(&mut sql);
                    self.statements(&before);
                    self.include(arg, dir);
                }
                continue;
            }
            let lower = t.to_lowercase();
            if lower.starts_with("copy ") && lower.contains(" from stdin") {
                copy = true;
            }
            sql.push_str(&ends_statement(line));
            sql.push('\n');
        }
        self.statements(&sql);
    }

    fn include(&mut self, file: String, dir: String) {
        if file.is_empty() || self.depth >= DEPTH {
            self.a.unread += 1;
            return;
        }
        let p = path(&dir, &file);
        match fs::read_to_string(&p) {
            Ok(text) => {
                let here = Path::new(&p).parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
                self.a.comments.push('\n');
                self.a.comments.push_str(&sql_comments(&without_copy(&text)));
                self.depth += 1;
                self.read(&text, &here);
                self.depth -= 1;
            }
            Err(_) => self.a.unread += 1,
        }
    }

    /// SQL split at its semicolons, each statement read on its own - one that is not
    /// read costs no other; a text the tokenizer cannot read, by its lines' semicolons
    fn statements(&mut self, sql: &str) {
        if sql.trim().is_empty() {
            return;
        }
        let dialect = PostgreSqlDialect {};
        match Tokenizer::new(&dialect, sql).tokenize() {
            Ok(tokens) => {
                for s in tokens.split(|t| *t == Token::SemiColon) {
                    self.one(s.to_vec());
                }
            }
            Err(_) => {
                let mut part = String::new();
                for line in sql.lines() {
                    part.push_str(line);
                    part.push('\n');
                    if line.trim_end().ends_with(';') {
                        self.chunk(&std::mem::take(&mut part));
                    }
                }
                self.chunk(&part);
            }
        }
    }

    fn chunk(&mut self, sql: &str) {
        if sql.trim().is_empty() {
            return;
        }
        match Tokenizer::new(&PostgreSqlDialect {}, sql).tokenize() {
            Ok(tokens) => {
                for s in tokens.split(|t| *t == Token::SemiColon) {
                    self.one(s.to_vec());
                }
            }
            Err(_) => self.a.unread += 1,
        }
    }

    /// one statement: parsed by sqlparser - all of it; a DO block by the SQL in its body;
    /// ALTER SEQUENCE and CREATE INDEX ON ONLY, which sqlparser does not read, by their words
    fn one(&mut self, tokens: Vec<Token>) {
        let words: Vec<&Token> = tokens.iter().filter(|t| !matches!(t, Token::Whitespace(_))).collect();
        if words.is_empty() {
            return;
        }
        if word(words[0]) == "do" {
            return self.do_block(&words);
        }
        match parse(tokens.clone()) {
            Some(s) => self.statement(&s),
            None => {
                if !self.by_words(&words) {
                    self.a.unread += 1;
                }
            }
        }
    }

    /// DO's body, plpgsql, read for its statements - one that changes and is not read is
    /// counted, a DO with none read too
    fn do_block(&mut self, words: &[&Token]) {
        let body = words.iter().find_map(|t| match t {
            Token::DollarQuotedString(d) => Some(d.value.clone()),
            Token::SingleQuotedString(s) | Token::EscapedStringLiteral(s) => Some(s.clone()),
            _ => None,
        });
        let unread = self.a.unread;
        let steps = match body {
            Some(b) => self.plpgsql(&Tokenizer::new(&PostgreSqlDialect {}, &b).tokenize().unwrap_or_default()),
            None => vec![],
        };
        if !any_change(&steps) && self.a.unread == unread {
            self.a.unread += 1;
        }
        self.a.steps.extend(steps);
    }

    /// [plan] plpgsql's statements, as steps of their own: an IF ... THEN ... [ELSE ...]
    /// END IF the database is asked about once the run is over; an IF with an ELSIF, its
    /// statements as they come
    fn plpgsql(&mut self, ts: &[Token]) -> Vec<Step> {
        let outer = std::mem::take(&mut self.a.steps);
        let (mut i, mut start, mut depth) = (0, 0, 0i32);
        // a statement can start here: at the start, after ; THEN ELSE BEGIN LOOP
        let mut opens = true;
        while i < ts.len() {
            match &ts[i] {
                Token::Whitespace(_) => {
                    i += 1;
                    continue;
                }
                Token::LParen => depth += 1,
                Token::RParen => depth -= 1,
                Token::SemiColon if depth == 0 => {
                    self.plpgsql_statement(&ts[start..i]);
                    i += 1;
                    start = i;
                    opens = true;
                    continue;
                }
                _ => {}
            }
            let w = word(&ts[i]);
            if depth == 0 && opens && w == "if" {
                if let Some(p) = if_parts(ts, i) {
                    self.plpgsql_statement(&ts[start..i]);
                    let then = self.plpgsql(&ts[p.then]);
                    let otherwise = self.plpgsql(&ts[p.otherwise]);
                    let cond = sql_text(&ts[p.cond]).trim().to_string();
                    self.a.steps.push(Step::When { cond, path: self.path.clone(), then, otherwise });
                    i = p.end;
                    start = p.end;
                    opens = true;
                    continue;
                }
            }
            opens = depth == 0 && OPENS.contains(&w.as_str());
            i += 1;
        }
        self.plpgsql_statement(&ts[start..]);
        std::mem::replace(&mut self.a.steps, outer)
    }

    /// one plpgsql statement: the SQL each EXECUTE '...' runs - schema_guard's retry wraps
    /// its DDL so - else the statement from its first word that changes something. One
    /// that starts so, or runs SQL of its own making, and is not read is counted
    fn plpgsql_statement(&mut self, ts: &[Token]) {
        let ws: Vec<&Token> = ts.iter().filter(|t| !matches!(t, Token::Whitespace(_))).collect();
        let (mut executed, mut made) = (false, false);
        for w in ws.windows(2) {
            if word(w[0]) == "execute" {
                match w[1] {
                    Token::SingleQuotedString(s) | Token::EscapedStringLiteral(s) => {
                        executed = true;
                        self.statements(s);
                    }
                    Token::DollarQuotedString(d) => {
                        executed = true;
                        self.statements(&d.value);
                    }
                    _ => made = true,
                }
            }
        }
        if executed {
            return;
        }
        match ts.iter().position(|t| CHANGES.contains(&word(t).as_str())) {
            Some(i) => match parse(ts[i..].to_vec()) {
                Some(st) => self.statement(&st),
                None => {
                    // a statement's own first word, or one after BEGIN THEN ELSE LOOP
                    let at = ws.iter().position(|t| CHANGES.contains(&word(t).as_str())).unwrap_or(0);
                    if at == 0 || OPENS.contains(&word(ws[at - 1]).as_str()) {
                        self.a.unread += 1;
                    }
                }
            },
            None if made => self.a.unread += 1,
            None => {}
        }
    }

    /// what sqlparser does not read, by its words: a statement that changes nothing -
    /// SELECT; - ALTER SEQUENCE ... OWNED BY a table's column, CREATE INDEX ... ON ONLY
    /// a table. true when it was one
    fn by_words(&mut self, w: &[&Token]) -> bool {
        let lower: Vec<String> = w.iter().map(|t| word(t)).collect();
        if NO_CHANGE.contains(&lower[0].as_str()) {
            return true;
        }
        if lower.len() > 2 && lower[0] == "alter" && lower[1] == "sequence" {
            let (seq, _) = self.name_at(w, 2);
            if let Some(i) = lower.windows(2).position(|p| p[0] == "owned" && p[1] == "by") {
                let (col, _) = self.name_at(w, i + 2);
                if let Some((t, _)) = col.rsplit_once('.') {
                    self.owners.sequences.insert(seq.clone(), t.to_string());
                    self.ddl(t.to_string(), format!("sequence {} owned", seq));
                    return true;
                }
            }
            self.orphan(seq.clone(), format!("alter sequence {}", seq));
            return true;
        }
        if lower.first().map(String::as_str) == Some("create") && lower.contains(&"index".to_string()) {
            if let Some(on) = lower.iter().position(|x| x == "on") {
                let at = if lower.get(on + 1).map(String::as_str) == Some("only") { on + 2 } else { on + 1 };
                let (t, _) = self.name_at(w, at);
                let i = lower.iter().position(|x| x == "index").unwrap_or(0) + 1;
                let named = lower.get(i).map_or(false, |x| !["on", "concurrently", "if"].contains(&x.as_str()));
                self.ddl(t, if named { format!("index {}", word_raw(w[i])) } else { "index".into() });
                return true;
            }
        }
        false
    }

    /// a name of words and dots, from token i: schema.table, its schema when it has none
    fn name_at(&self, w: &[&Token], mut i: usize) -> (String, usize) {
        let mut parts = vec![];
        while let Some(Token::Word(x)) = w.get(i) {
            parts.push(if x.quote_style.is_none() { x.value.to_lowercase() } else { x.value.clone() });
            if w.get(i + 1) == Some(&&Token::Period) {
                i += 2;
            } else {
                i += 1;
                break;
            }
        }
        let n = parts.join(".");
        (if parts.len() == 1 { format!("{}.{}", self.schema, n) } else { n }, i)
    }

    fn ddl(&mut self, table: String, what: String) {
        self.ddl_if(Target::Table(table), what, vec![], vec![]);
    }

    /// [plan] DDL on its conditions - each object there, or not - and what it makes
    fn ddl_if(&mut self, on: Target, what: String, when: When, made: Made) {
        self.a.steps.push(Step::Ddl { on, what, when, made });
    }

    /// an index's or a sequence's table: the run's, as far as it is read - else told once
    /// the run is over
    fn on_of(&self, object: &str) -> Target {
        match self.owners.table(object) {
            Some(t) => Target::Table(t),
            None => Target::Of(object.into()),
        }
    }

    fn orphan(&mut self, object: String, what: String) {
        let on = self.on_of(&object);
        self.ddl_if(on, what, vec![], vec![]);
    }

    /// a name as Postgres has it: unquoted in lower case, in its schema - SET
    /// search_path's, else public - when it names none; such a one, under a search_path
    /// of more than one, kept to be told where the database has it
    fn name(&self, n: &ObjectName) -> String {
        let parts: Vec<String> = n.0.iter().map(|p| match p {
            ObjectNamePart::Identifier(i) => ident(i),
            o => o.to_string(),
        }).collect();
        if parts.len() > 1 {
            return parts.join(".");
        }
        let read = format!("{}.{}", self.schema, parts[0]);
        if let Some(p) = self.path.as_ref().filter(|p| schemas(p).len() > 1) {
            self.bare.borrow_mut().insert((read.clone(), parts[0].clone(), p.clone()));
        }
        read
    }

    /// [plan] how the database finds a name: by the schema it is written with; without
    /// one, under the search_path - in its first schema for a CREATE, else the first it is in
    fn find(&self, n: &ObjectName, create: bool) -> Lookup {
        if n.0.len() > 1 {
            Lookup::Schema
        } else if create {
            Lookup::First(self.path.clone())
        } else {
            Lookup::Any(self.path.clone())
        }
    }

    fn relation_find(&self, t: &TableFactor) -> Lookup {
        match t {
            TableFactor::Table { name: n, .. } => self.find(n, false),
            _ => Lookup::Schema,
        }
    }

    /// a sequence a column's default takes values from - nextval('...') - is the table's
    fn defaults(&mut self, table: &str, text: &str) {
        let mut from = 0;
        while let Some(i) = text[from..].find("nextval('") {
            let start = from + i + 9;
            let end = text[start..].find('\'').map_or(text.len(), |e| start + e);
            let seq = &text[start..end];
            let seq = if seq.contains('.') { seq.to_lowercase() } else { format!("{}.{}", self.schema, seq.to_lowercase()) };
            self.owners.sequences.insert(seq, table.to_string());
            from = end;
        }
    }

    /// one statement's change, on its table: DDL with its columns - on its IF [NOT]
    /// EXISTS - and DML's rows, counted. An index, a sequence, a trigger, a policy on the
    /// table they belong to; SELECT, SET, transactions, COPY and INSERT ... SELECT change
    /// nothing the plan reads
    fn statement(&mut self, s: &Statement) {
        match s {
            Statement::CreateTable(t) => {
                let table = self.name(&t.name);
                for c in &t.columns {
                    self.defaults(&table, &c.to_string());
                    let ty = c.data_type.to_string().to_lowercase();
                    if ty.contains("serial") {
                        let (schema, tn) = table.rsplit_once('.').unwrap_or(("public", &table));
                        self.owners.sequences.insert(format!("{}.{}_{}_seq", schema, tn, ident(&c.name)), table.clone());
                    }
                }
                let me = Object::Relation(table.clone());
                let when = if t.if_not_exists { vec![(me.clone(), false, self.find(&t.name, true))] } else { vec![] };
                let mut made = vec![(me, true)];
                made.extend(t.columns.iter().map(|c| (Object::Column(table.clone(), ident(&c.name)), true)));
                let what = format!("create ({})", list(t.columns.iter().map(|c| ident(&c.name))));
                self.ddl_if(Target::Table(table), what, when, made);
            }
            Statement::AlterTable(a) => {
                let table = self.name(&a.name);
                let find = self.find(&a.name, false);
                let base: When = if a.if_exists { vec![(Object::Relation(table.clone()), true, find.clone())] } else { vec![] };
                let ops: Vec<(String, When, Made)> = a.operations.iter().flat_map(|op| self.alter(&table, &find, op)).collect();
                if ops.iter().all(|(_, when, _)| when.is_empty()) {
                    let what = format!("alter: {}", ops.iter().map(|(w, _, _)| w.as_str()).collect::<Vec<_>>().join(", "));
                    let made = ops.into_iter().flat_map(|(_, _, m)| m).collect();
                    self.ddl_if(Target::Table(table), what, base, made);
                } else {
                    // an operation on a condition of its own is a change of its own
                    for (what, when, made) in ops {
                        let when = base.iter().cloned().chain(when).collect();
                        self.ddl_if(Target::Table(table.clone()), format!("alter: {}", what), when, made);
                    }
                }
            }
            Statement::CreateIndex(i) => {
                let table = self.name(&i.table_name);
                let mut when = vec![];
                let mut made = vec![];
                if let Some(n) = &i.name {
                    // the index is in its table's schema
                    let schema = table.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_default();
                    let key = if n.0.len() == 1 { format!("{}.{}", schema, last_part(n)) } else { written(n) };
                    self.owners.indexes.insert(key.clone(), table.clone());
                    if i.if_not_exists {
                        let find = if n.0.len() > 1 { Lookup::Schema } else { self.find(&i.table_name, false) };
                        when.push((Object::Relation(key.clone()), false, find));
                    }
                    made.push((Object::Relation(key), true));
                }
                let what = format!("index {}({})",
                    i.name.as_ref().map(|n| format!("{} ", last_part(n))).unwrap_or_default(),
                    list(i.columns.iter().map(|c| c.column.expr.to_string())));
                self.ddl_if(Target::Table(table), what, when, made);
            }
            Statement::AlterIndex { name, operation } => {
                let n = self.name(name);
                let AlterIndexOperation::RenameIndex { index_name } = operation;
                let schema = n.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_default();
                let to = format!("{}.{}", schema, last_part(index_name));
                let made = vec![(Object::Relation(n.clone()), false), (Object::Relation(to), true)];
                let on = self.on_of(&n);
                self.ddl_if(on, format!("alter index {}", last_part(name)), vec![], made);
            }
            Statement::CreateSequence { name: n, owned_by, if_not_exists, .. } => {
                let seq = self.name(n);
                let me = Object::Relation(seq.clone());
                let when = if *if_not_exists { vec![(me.clone(), false, self.find(n, true))] } else { vec![] };
                match owned_by.as_ref().map(|o| self.name(o)) {
                    Some(col) => {
                        let t = col.rsplit_once('.').map(|(t, _)| t.to_string()).unwrap_or_default();
                        self.owners.sequences.insert(seq.clone(), t.clone());
                        self.ddl_if(Target::Table(t), format!("sequence {}", seq), when, vec![(me, true)]);
                    }
                    None => {
                        let on = self.on_of(&seq);
                        self.ddl_if(on, format!("sequence {}", seq), when, vec![(me, true)]);
                    }
                }
            }
            Statement::CreateView(v) => {
                let t = self.name(&v.name);
                let me = Object::Relation(t.clone());
                let when = if v.if_not_exists { vec![(me.clone(), false, self.find(&v.name, true))] } else { vec![] };
                let what = if v.materialized { "create materialized view" } else { "create view" };
                self.ddl_if(Target::Table(t), what.into(), when, vec![(me, true)]);
            }
            Statement::CreateTrigger(t) => {
                let table = self.name(&t.table_name);
                let name = last_part(&t.name);
                let made = vec![(Object::Trigger(table.clone(), name.clone()), true)];
                self.ddl_if(Target::Table(table), format!("trigger {}", name), vec![], made);
            }
            Statement::DropTrigger(t) => {
                let table = t.table_name.as_ref().map(|n| self.name(n)).unwrap_or_default();
                let find = t.table_name.as_ref().map_or(Lookup::Schema, |n| self.find(n, false));
                let name = last_part(&t.trigger_name);
                let me = Object::Trigger(table.clone(), name.clone());
                let when = if t.if_exists { vec![(me.clone(), true, find)] } else { vec![] };
                self.ddl_if(Target::Table(table), format!("drop trigger {}", name), when, vec![(me, false)]);
            }
            Statement::CreatePolicy(p) => {
                let table = self.name(&p.table_name);
                let made = vec![(Object::Policy(table.clone(), ident(&p.name)), true)];
                self.ddl_if(Target::Table(table), format!("policy {}", ident(&p.name)), vec![], made);
            }
            Statement::DropPolicy(p) => {
                let table = self.name(&p.table_name);
                let me = Object::Policy(table.clone(), ident(&p.name));
                let when = if p.if_exists { vec![(me.clone(), true, self.find(&p.table_name, false))] } else { vec![] };
                self.ddl_if(Target::Table(table), format!("drop policy {}", ident(&p.name)), when, vec![(me, false)]);
            }
            Statement::DropFunction(f) => for d in &f.func_desc {
                let me = Object::Function(self.name(&d.name));
                let when = if f.if_exists { vec![(me.clone(), true, self.find(&d.name, false))] } else { vec![] };
                self.ddl_if(Target::Table(OTHER.into()), format!("drop function {}", written(&d.name)), when, vec![(me, false)]);
            },
            Statement::CreateFunction(f) => {
                let made = vec![(Object::Function(self.name(&f.name)), true)];
                self.ddl_if(Target::Table(OTHER.into()), words(&s.to_string(), 3), vec![], made);
            }
            Statement::CreateType { name, .. } => {
                let made = vec![(Object::Type(self.name(name)), true)];
                self.ddl_if(Target::Table(OTHER.into()), words(&s.to_string(), 3), vec![], made);
            }
            Statement::Drop { object_type, if_exists, names, table, .. } => for n in names {
                let find = self.find(n, false);
                let gone = |o: Object, find: Lookup| -> (When, Made) {
                    (if *if_exists { vec![(o.clone(), true, find)] } else { vec![] }, vec![(o, false)])
                };
                match object_type {
                    ObjectType::Table | ObjectType::View | ObjectType::MaterializedView => {
                        let t = self.name(n);
                        let (when, made) = gone(Object::Relation(t.clone()), find);
                        self.ddl_if(Target::Table(t), "drop".into(), when, made);
                    }
                    ObjectType::Index => {
                        let o = self.name(n);
                        let (when, made) = gone(Object::Relation(o.clone()), find);
                        let on = match table {
                            Some(t) => Target::Table(self.name(t)),
                            None => self.on_of(&o),
                        };
                        self.ddl_if(on, format!("drop index {}", last_part(n)), when, made);
                    }
                    ObjectType::Sequence => {
                        let o = self.name(n);
                        let (when, made) = gone(Object::Relation(o.clone()), find);
                        let on = self.on_of(&o);
                        self.ddl_if(on, format!("drop sequence {}", o), when, made);
                    }
                    ObjectType::Schema => {
                        let (when, made) = gone(Object::Schema(written(n)), Lookup::Schema);
                        self.ddl_if(Target::Table(OTHER.into()), format!("drop schema {}", written(n)), when, made);
                    }
                    ObjectType::Type => {
                        let (when, made) = gone(Object::Type(self.name(n)), find);
                        self.ddl_if(Target::Table(OTHER.into()), format!("drop type {}", written(n)), when, made);
                    }
                    o => {
                        let what = format!("drop {} {}", o.to_string().to_lowercase(), written(n));
                        self.ddl(OTHER.into(), what);
                    }
                }
            },
            Statement::CreateSchema { schema_name, if_not_exists, .. } => {
                let name = match schema_name {
                    SchemaName::Simple(n) | SchemaName::NamedAuthorization(n, _) => written(n),
                    SchemaName::UnnamedAuthorization(i) => ident(i),
                };
                let me = Object::Schema(name.clone());
                let when = if *if_not_exists { vec![(me.clone(), false, Lookup::Schema)] } else { vec![] };
                self.ddl_if(Target::Table(OTHER.into()), format!("create schema {}", name), when, vec![(me, true)]);
            }
            Statement::CreateExtension(x) => {
                let me = Object::Extension(ident(&x.name));
                let when = if x.if_not_exists { vec![(me.clone(), false, Lookup::Schema)] } else { vec![] };
                self.ddl_if(Target::Table(OTHER.into()), format!("create extension {}", ident(&x.name)), when, vec![(me, true)]);
            }
            Statement::DropExtension(x) => for n in &x.names {
                let me = Object::Extension(ident(n));
                let when = if x.if_exists { vec![(me.clone(), true, Lookup::Schema)] } else { vec![] };
                self.ddl_if(Target::Table(OTHER.into()), format!("drop extension {}", ident(n)), when, vec![(me, false)]);
            },
            Statement::Comment { object_type, object_name, .. } => match object_type {
                CommentObject::Table | CommentObject::View | CommentObject::MaterializedView => {
                    let t = self.name(object_name);
                    self.ddl(t, "comment".into());
                }
                CommentObject::Column => {
                    let full = self.name(object_name);
                    let (t, c) = full.rsplit_once('.').map(|(t, c)| (t.to_string(), c.to_string())).unwrap_or_default();
                    let t = if t.contains('.') { t } else { format!("{}.{}", self.schema, t) };
                    self.ddl(t, format!("comment {}", c));
                }
                CommentObject::Index | CommentObject::Sequence => {
                    let o = self.name(object_name);
                    self.orphan(o, format!("comment {} {}", object_type.to_string().to_lowercase(), last_part(object_name)));
                }
                o => {
                    let what = format!("comment on {} {}", o.to_string().to_lowercase(), written(object_name));
                    self.ddl(OTHER.into(), what);
                }
            },
            Statement::Grant(g) => self.grant("grant", &g.objects, &s.to_string()),
            Statement::Revoke(r) => self.grant("revoke", &r.objects, &s.to_string()),
            // [plan] TRUNCATE: the rows each table has, counted
            Statement::Truncate(t) => for tt in &t.table_names {
                let table = self.name(&tt.name);
                let find = self.find(&tt.name, false);
                let sql = Some(format!("SELECT count(*) FROM {}{}", if tt.only { "ONLY " } else { "" }, tt.name));
                self.a.steps.push(Step::Count { table, find, dml: Dml::Truncate, sql, path: self.path.clone() });
            },
            Statement::Insert(i) => self.insert(i),
            Statement::Update(u) => {
                let table = self.relation(&u.table.relation);
                let find = self.relation_find(&u.table.relation);
                let sql = self.count_update(u);
                self.a.steps.push(Step::Count { table, find, dml: Dml::Update, sql, path: self.path.clone() });
            }
            Statement::Delete(d) => {
                let from = match &d.from {
                    FromTable::WithFromKeyword(v) | FromTable::WithoutKeyword(v) => v,
                };
                let table = from.first().map(|t| self.relation(&t.relation)).unwrap_or_default();
                let find = from.first().map_or(Lookup::Schema, |t| self.relation_find(&t.relation));
                let sql = match from.as_slice() {
                    [one] if one.joins.is_empty() && d.tables.is_empty() && d.limit.is_none() && d.order_by.is_empty()
                        && !self.with_writes => Some(count_sql(self.with.as_deref(), &one.relation.to_string(),
                            d.using.as_ref().map(|u| join(u)), d.selection.as_ref())),
                    _ => None,
                };
                self.a.steps.push(Step::Count { table, find, dml: Dml::Delete, sql, path: self.path.clone() });
            }
            Statement::Merge(m) => {
                let table = self.relation(&m.table);
                let find = self.relation_find(&m.table);
                self.a.steps.push(Step::Count { table, find, dml: Dml::Merge, sql: None, path: self.path.clone() });
            }
            // [plan] COPY is not read
            Statement::Copy { .. } => {}
            Statement::Query(q) => self.query(q),
            o => {
                let text = o.to_string();
                let w = words(&text, 3);
                let first = w.split(' ').next().unwrap_or("").to_string();
                let lower = text.to_lowercase();
                if first == "set" && lower.contains("search_path") {
                    self.search_path(&text);
                } else if first == "reset" && (lower.contains("search_path") || lower.contains(" all")) {
                    self.schema = "public".into();
                    self.path = None;
                } else if !NO_CHANGE.contains(&first.as_str()) {
                    self.ddl(OTHER.into(), w);
                }
            }
        }
    }

    /// [plan] an ALTER TABLE's operation: what it does, on its condition - IF [NOT]
    /// EXISTS, the table found as `find` - and what it makes and drops. DROP COLUMN IF
    /// EXISTS a, b: a, b each
    fn alter(&mut self, table: &str, find: &Lookup, op: &AlterTableOperation) -> Vec<(String, When, Made)> {
        let col = |c: &Ident| Object::Column(table.to_string(), ident(c));
        let schema = table.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_default();
        match op {
            AlterTableOperation::AddColumn { column_def, if_not_exists, .. } => {
                self.defaults(table, &column_def.to_string());
                let me = col(&column_def.name);
                let when = if *if_not_exists { vec![(me.clone(), false, find.clone())] } else { vec![] };
                vec![(format!("add {}", ident(&column_def.name)), when, vec![(me, true)])]
            }
            AlterTableOperation::DropColumn { column_names, if_exists, .. } => {
                if *if_exists {
                    column_names.iter().map(|c| (format!("drop {}", ident(c)), vec![(col(c), true, find.clone())], vec![(col(c), false)]))
                        .collect()
                } else {
                    vec![(format!("drop {}", list(column_names.iter().map(ident))), vec![],
                        column_names.iter().map(|c| (col(c), false)).collect())]
                }
            }
            AlterTableOperation::AlterColumn { column_name, op } => {
                self.defaults(table, &op.to_string());
                vec![(format!("alter {}", ident(column_name)), vec![], vec![])]
            }
            AlterTableOperation::RenameColumn { old_column_name, new_column_name } =>
                vec![(format!("rename {} to {}", ident(old_column_name), ident(new_column_name)), vec![],
                    vec![(col(old_column_name), false), (col(new_column_name), true)])],
            AlterTableOperation::AddConstraint { constraint, .. } => {
                let text = constraint.to_string();
                let made = constraint_name(&text).map(|n| vec![(Object::Constraint(table.to_string(), n), true)]).unwrap_or_default();
                vec![(format!("add {}", words(&text, 6)), vec![], made)]
            }
            AlterTableOperation::DropConstraint { name, if_exists, .. } => {
                let me = Object::Constraint(table.to_string(), ident(name));
                let when = if *if_exists { vec![(me.clone(), true, find.clone())] } else { vec![] };
                vec![(format!("drop constraint {}", ident(name)), when, vec![(me, false)])]
            }
            AlterTableOperation::RenameConstraint { old_name, new_name } =>
                vec![(format!("rename constraint {} to {}", ident(old_name), ident(new_name)), vec![],
                    vec![(Object::Constraint(table.to_string(), ident(old_name)), false),
                        (Object::Constraint(table.to_string(), ident(new_name)), true)])],
            AlterTableOperation::RenameTable { table_name } => {
                // the new name is in the table's schema
                let (RenameTableNameKind::As(n) | RenameTableNameKind::To(n)) = table_name;
                let to = format!("{}.{}", schema, last_part(n));
                vec![(words(&op.to_string(), 6), vec![], vec![(Object::Relation(table.to_string()), false),
                    (Object::Relation(to.clone()), true), (Object::Renamed(to, table.to_string()), true)])]
            }
            o => vec![(words(&o.to_string(), 6), vec![], vec![])],
        }
    }

    /// [plan] an INSERT's rows: its VALUES' - ON CONFLICT (its key), each key once, less
    /// those the table has, and what it does with those; a conflict on a key it does not
    /// name, not counted - a DEFAULT VALUES's one, a SELECT of no table's: one, or as many
    /// as its WHERE lets through, counted. INSERT ... SELECT is not read
    fn insert(&mut self, i: &Insert) {
        let (table, find) = match &i.table {
            TableObject::TableName(n) => (self.name(n), self.find(n, false)),
            _ => (OTHER.into(), Lookup::Schema),
        };
        let path = self.path.clone();
        let rows = match i.source.as_ref().map(|q| q.body.as_ref()) {
            None => 1,
            Some(SetExpr::Values(v)) => v.rows.len() as u64,
            Some(SetExpr::Select(sel)) if sel.from.is_empty() => {
                if sel.selection.is_some() || sel.having.is_some() {
                    let sql = Some(format!("SELECT count(*) FROM ({}) AS s", sel));
                    self.a.steps.push(Step::Count { table, find, dml: Dml::Insert, sql, path });
                    return;
                }
                1
            }
            _ => return,
        };
        let step = match &i.on {
            Some(OnInsert::OnConflict(oc)) => match keyed(i, oc) {
                Some((rows, matched, conflict)) => Step::Keyed { table, find, rows, statements: 1, matched, conflict, deleted: vec![],
                    setval: false, path },
                None => Step::Count { table, find, dml: Dml::Insert, sql: None, path },
            },
            _ => Step::Rows { table, dml: Dml::Insert, n: rows },
        };
        self.a.steps.push(step);
    }

    /// [plan] SELECT count(*) of the rows an UPDATE changes - none when it cannot be told:
    /// a WITH that writes, a join on its table
    fn count_update(&self, u: &Update) -> Option<String> {
        if self.with_writes || !u.table.joins.is_empty() || u.limit.is_some() || !u.order_by.is_empty() {
            return None;
        }
        let from = match &u.from {
            Some(UpdateTableFromKind::AfterSet(f)) | Some(UpdateTableFromKind::BeforeSet(f)) => Some(join(f)),
            None => None,
        };
        Some(count_sql(self.with.as_deref(), &u.table.relation.to_string(), from, u.selection.as_ref()))
    }

    /// SET search_path TO a, b: names without a schema are a's, and the database is asked
    /// under a, b; TO DEFAULT, its own again
    fn search_path(&mut self, text: &str) {
        let lower = text.to_ascii_lowercase();
        let at = lower.find("search_path").map_or(text.len(), |i| i + "search_path".len());
        let rest = text[at..].trim_start().trim_start_matches('=').trim_start();
        let rest = match rest.get(..3) {
            Some(to) if to.eq_ignore_ascii_case("to ") => rest[3..].trim(),
            _ => rest.trim(),
        };
        if rest.is_empty() || rest.eq_ignore_ascii_case("default") {
            self.schema = "public".into();
            self.path = None;
            return;
        }
        if let Some(s) = schemas(rest).into_iter().next() {
            self.schema = s;
        }
        self.path = Some(rest.to_string());
    }

    /// a query that changes: WITH ... INSERT, UPDATE, DELETE, MERGE - counted with its
    /// WITH, unless that writes; a WITH's own, with the WITHs before it - SELECT ... INTO
    /// a new table; a sequence set to its table's rows - sge's data files. A query in it
    /// with no WITH of its own has its
    fn query(&mut self, q: &Query) {
        let outer = (self.with.clone(), self.with_writes);
        if let Some(w) = &q.with {
            for (k, cte) in w.cte_tables.iter().enumerate() {
                let before = &w.cte_tables[..k];
                self.with_writes = before.iter().any(|c| writes(&c.query.body));
                self.with = if before.is_empty() {
                    None
                } else {
                    Some(format!("WITH {}{}", if w.recursive { "RECURSIVE " } else { "" },
                        before.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", ")))
                };
                self.query(&cte.query);
            }
            self.with_writes = w.cte_tables.iter().any(|c| writes(&c.query.body));
            self.with = Some(w.to_string());
        }
        self.set_expr(&q.body);
        (self.with, self.with_writes) = outer;
    }

    fn set_expr(&mut self, e: &SetExpr) {
        match e {
            SetExpr::Insert(s) | SetExpr::Update(s) | SetExpr::Delete(s) | SetExpr::Merge(s) => self.statement(s),
            SetExpr::Query(q) => self.query(q),
            SetExpr::SetOperation { left, right, .. } => {
                self.set_expr(left);
                self.set_expr(right);
            }
            SetExpr::Select(sel) => {
                let table = sel.from.first().map(|f| self.relation(&f.relation));
                if let Some(into) = &sel.into {
                    if let Some(t) = into.targets.first() {
                        let n = t.to_string();
                        let n = if n.contains('"') { n } else { n.to_lowercase() };
                        let n = if n.contains('.') { n } else { format!("{}.{}", self.schema, n) };
                        self.ddl(n, "create (select into)".into());
                    }
                }
                let text = sel.to_string();
                if let Some(i) = text.find("setval(").or_else(|| text.find("SETVAL(")) {
                    let seq = text[i + 7..].trim_start().strip_prefix('\'').and_then(|r| r.split('\'').next()).unwrap_or("?").to_string();
                    let t = table.unwrap_or_default();
                    let key = if seq.contains('.') { seq.clone() } else { format!("{}.{}", self.schema, seq) };
                    if !t.is_empty() {
                        self.owners.sequences.insert(key, t.clone());
                    }
                    self.ddl(t, format!("sequence {} setval", seq));
                }
            }
            _ => {}
        }
    }

    /// GRANT's and REVOKE's tables; on another object, under other with its name
    fn grant(&mut self, what: &str, objects: &Option<GrantObjects>, text: &str) {
        match objects {
            Some(GrantObjects::Tables(ts)) => for t in ts {
                let n = self.name(t);
                self.ddl(n, what.into());
            },
            Some(o) => {
                let w = format!("{} on {}", what, words(&o.to_string(), 6));
                self.ddl(OTHER.into(), w);
            }
            None => {
                let w = words(text, 6);
                self.ddl(OTHER.into(), w);
            }
        }
    }

    fn relation(&self, t: &TableFactor) -> String {
        match t {
            TableFactor::Table { name: n, .. } => self.name(n),
            _ => OTHER.into(),
        }
    }
}

/// [plan] where a plpgsql statement can start: after these words
const OPENS: [&str; 4] = ["then", "else", "begin", "loop"];

/// a statement, all of its tokens read. With words left over: a DML, a DROP, an ALTER
/// TABLE is not read - a WHERE, a list cut short would tell what it does not do; another
/// DDL is, its options after its name and columns telling the plan nothing
fn parse(tokens: Vec<Token>) -> Option<Statement> {
    let dialect = PostgreSqlDialect {};
    let mut p = Parser::new(&dialect).with_tokens(tokens);
    let s = p.parse_statement().ok()?;
    while p.peek_token().token == Token::SemiColon {
        p.next_token();
    }
    if p.peek_token().token == Token::EOF {
        return Some(s);
    }
    match s {
        Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_) | Statement::Merge(_) | Statement::Query(_)
        | Statement::Copy { .. } | Statement::Drop { .. } | Statement::AlterTable(_) => None,
        s => Some(s),
    }
}

/// [plan] steps that change something - an IF's, in either branch
fn any_change(steps: &[Step]) -> bool {
    steps.iter().any(|s| match s {
        Step::When { then, otherwise, .. } => any_change(then) || any_change(otherwise),
        _ => true,
    })
}

/// [plan] tokens as SQL the database reads: a string quoted again, a comment a space
fn sql_text(ts: &[Token]) -> String {
    ts.iter().map(|t| match t {
        Token::SingleQuotedString(s) => format!("'{}'", s.replace('\'', "''")),
        Token::EscapedStringLiteral(s) => format!("E'{}'", s.replace('\\', "\\\\").replace('\'', "\\'")),
        Token::Whitespace(Whitespace::SingleLineComment { .. }) | Token::Whitespace(Whitespace::MultiLineComment(_)) => " ".into(),
        o => o.to_string(),
    }).collect()
}

/// a constraint's name: CONSTRAINT name ... - as Postgres has it
fn constraint_name(text: &str) -> Option<String> {
    let mut w = text.split_whitespace();
    if !w.next()?.eq_ignore_ascii_case("constraint") {
        return None;
    }
    let n = w.next()?;
    Some(match n.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(q) => q.to_string(),
        None => n.to_lowercase(),
    })
}

/// [plan] SELECT count(*) of the rows a statement changes: those of its table its WHERE
/// selects - with a FROM or a USING, those it finds a row there for
fn count_sql(with: Option<&str>, table: &str, from: Option<String>, filter: Option<&Expr>) -> String {
    let mut sql = with.map(|w| format!("{} ", w)).unwrap_or_default();
    sql.push_str(&format!("SELECT count(*) FROM {}", table));
    match (from, filter) {
        (Some(f), w) => sql.push_str(&format!(" WHERE EXISTS (SELECT 1 FROM {}{})", f,
            w.map(|w| format!(" WHERE {}", w)).unwrap_or_default())),
        (None, Some(w)) => sql.push_str(&format!(" WHERE {}", w)),
        (None, None) => {}
    }
    sql
}

fn join(tables: &[TableWithJoins]) -> String {
    tables.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
}

/// a query that writes: a WITH's INSERT, UPDATE, DELETE, MERGE
fn writes(e: &SetExpr) -> bool {
    match e {
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => true,
        SetExpr::Query(q) => writes(&q.body),
        SetExpr::SetOperation { left, right, .. } => writes(left) || writes(right),
        _ => false,
    }
}

/// [plan] an INSERT's keys - ON CONFLICT (its columns), each named in its column list,
/// its VALUES literals - each once; SELECT count(*) of those the table has, so many at a
/// time; and what it does with those. None when they cannot be told
fn keyed(i: &Insert, oc: &OnConflict) -> Option<(u64, Vec<String>, Conflict)> {
    let key = match &oc.conflict_target {
        Some(ConflictTarget::Columns(c)) if !c.is_empty() => c,
        _ => return None,
    };
    let names: Vec<String> = i.columns.iter().map(last_part).collect();
    let at: Vec<usize> = key.iter().map(|k| names.iter().position(|n| *n == ident(k))).collect::<Option<_>>()?;
    let rows = match i.source.as_ref().map(|q| q.body.as_ref()) {
        Some(SetExpr::Values(v)) => &v.rows,
        _ => return None,
    };
    let mut keys = BTreeSet::new();
    for r in rows {
        let vals: Vec<String> = at.iter().map(|&p| r.get(p).filter(|e| literal(e)).map(|e| e.to_string())).collect::<Option<_>>()?;
        keys.insert(if vals.len() == 1 { vals[0].clone() } else { format!("({})", vals.join(", ")) });
    }
    let cols: Vec<String> = key.iter().map(|k| k.to_string()).collect();
    let lhs = if cols.len() == 1 { cols[0].clone() } else { format!("({})", cols.join(", ")) };
    let keys: Vec<String> = keys.into_iter().collect();
    let matched = keys.chunks(KEYS)
        .map(|ch| format!("SELECT count(*) FROM {} WHERE {} IN ({})", i.table, lhs, ch.join(", ")))
        .collect();
    let conflict = match &oc.action {
        OnConflictAction::DoUpdate(u) if u.selection.is_some() => Conflict::UpdateWhere,
        OnConflictAction::DoUpdate(_) => Conflict::Update,
        _ => Conflict::Nothing,
    };
    Some((keys.len() as u64, matched, conflict))
}

/// a literal: a value, signed, cast, in parentheses
fn literal(e: &Expr) -> bool {
    match e {
        Expr::Value(_) => true,
        Expr::UnaryOp { expr, .. } | Expr::Cast { expr, .. } | Expr::Nested(expr) => literal(expr),
        _ => false,
    }
}

/// [plan] an IF's parts, from its IF at `at`: none with an ELSIF, or no END IF. A CASE's
/// THEN, ELSE and END in it are the CASE's, a block's END the block's; an IF in it, and its
/// END IF, its own
fn if_parts(ts: &[Token], at: usize) -> Option<IfParts> {
    let (mut depth, mut cases) = (0i32, 0usize);
    let mut i = at + 1;
    let then = loop {
        let t = ts.get(i)?;
        match t {
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }
        match word(t).as_str() {
            "case" if depth == 0 => cases += 1,
            "end" if depth == 0 && cases > 0 => cases -= 1,
            "then" if depth == 0 && cases == 0 => break i,
            _ => {}
        }
        i += 1;
    };
    // the CASEs - true - and BEGIN blocks open in it, inner last
    let mut open: Vec<bool> = vec![];
    let (mut nest, mut opens, mut els) = (0usize, true, None);
    i = then + 1;
    loop {
        let t = ts.get(i)?;
        let w = word(t);
        match t {
            Token::Whitespace(_) => {
                i += 1;
                continue;
            }
            Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            _ => {}
        }
        if depth == 0 {
            match w.as_str() {
                "case" => open.push(true),
                "begin" => open.push(false),
                "end" => {
                    let next = next_word(ts, i + 1).map(|j| (j, word(&ts[j])));
                    match next {
                        Some((j, n)) if n == "if" => {
                            if nest == 0 {
                                // after END IF and its ;
                                let mut end = j + 1;
                                if let Some(k) = next_word(ts, end) {
                                    if ts[k] == Token::SemiColon {
                                        end = k + 1;
                                    }
                                }
                                return Some(IfParts {
                                    cond: at + 1..then,
                                    then: then + 1..els.unwrap_or(i),
                                    otherwise: els.map_or(i..i, |e| e + 1..i),
                                    end,
                                });
                            }
                            nest -= 1;
                            i = j + 1;
                            opens = false;
                            continue;
                        }
                        Some((j, n)) if n == "case" || n == "loop" => {
                            if n == "case" && open.last() == Some(&true) {
                                open.pop();
                            }
                            i = j + 1;
                            opens = false;
                            continue;
                        }
                        // a CASE's END, or a block's
                        _ => {
                            open.pop();
                        }
                    }
                }
                "if" if opens => nest += 1,
                "elsif" | "elseif" if nest == 0 && open.is_empty() => return None,
                "else" if nest == 0 && open.is_empty() && els.is_none() => els = Some(i),
                _ => {}
            }
        }
        opens = matches!(t, Token::SemiColon) || OPENS.contains(&w.as_str());
        i += 1;
    }
}

/// the next token from i that is no whitespace
fn next_word(ts: &[Token], i: usize) -> Option<usize> {
    (i..ts.len()).find(|&k| !matches!(ts[k], Token::Whitespace(_)))
}

/// a statement ended where psql ends it: a \g, \gset, \gexec, \watch at a line's end
fn ends_statement(line: &str) -> String {
    if let Some(i) = line.rfind('\\') {
        let cmd = &line[i + 1..];
        if (cmd.starts_with('g') || cmd.starts_with("watch")) && !cmd.contains('\'') && !cmd.contains('"')
            && !line[..i].trim().is_empty() {
            return format!("{};", &line[..i]);
        }
    }
    line.to_string()
}

/// a token's word in lower case; "" for any other token
fn word(t: &Token) -> String {
    match t {
        Token::Word(w) => w.value.to_lowercase(),
        _ => String::new(),
    }
}

fn word_raw(t: &Token) -> String {
    match t {
        Token::Word(w) if w.quote_style.is_none() => w.value.to_lowercase(),
        Token::Word(w) => w.value.clone(),
        o => o.to_string(),
    }
}

/// a name as Postgres has it: unquoted, in lower case
fn ident(i: &Ident) -> String {
    if i.quote_style.is_none() { i.value.to_lowercase() } else { i.value.clone() }
}

/// a name as written, no schema put to it: a schema's, a role's, an extension's are in
/// none
fn written(n: &ObjectName) -> String {
    n.0.iter().map(|p| match p {
        ObjectNamePart::Identifier(i) => ident(i),
        o => o.to_string(),
    }).collect::<Vec<_>>().join(".")
}

fn last_part(n: &ObjectName) -> String {
    n.0.last().map(|p| match p {
        ObjectNamePart::Identifier(i) => ident(i),
        o => o.to_string(),
    }).unwrap_or_default()
}

/// a list, its first COLUMNS and the count of the rest
fn list<I: Iterator<Item = String>>(items: I) -> String {
    let all: Vec<String> = items.collect();
    if all.len() > COLUMNS {
        format!("{}, +{}", all[..COLUMNS].join(", "), all.len() - COLUMNS)
    } else {
        all.join(", ")
    }
}

/// a statement's first words, in lower case - a name in quotes as it is - up to
/// REFERENCES or the first parenthesis
fn words(text: &str, n: usize) -> String {
    let mut out = vec![];
    for w in text.split_whitespace() {
        if out.len() == n || w.eq_ignore_ascii_case("references") {
            break;
        }
        let head = w.split('(').next().unwrap_or("");
        let skip = ["if", "not", "exists", "or", "replace"].contains(&head.to_lowercase().as_str());
        if !head.is_empty() && !skip {
            out.push(if head.contains('"') { head.to_string() } else { head.to_lowercase() });
        }
        if w.contains('(') {
            break;
        }
    }
    out.join(" ")
}

/// text for md: its marks as text, an _ only where it could start or end emphasis
fn esc(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    for (i, ch) in c.iter().enumerate() {
        let edge = |j: Option<&char>| j.map_or(true, |x| !x.is_alphanumeric());
        let mark = matches!(ch, '\\' | '*' | '`' | '|' | '<' | '>' | '[' | ']')
            || (*ch == '_' && (edge(if i == 0 { None } else { c.get(i - 1) }) || edge(c.get(i + 1))));
        if mark {
            out.push('\\');
        }
        out.push(*ch);
    }
    out
}

/// a file name as code: in a fence of more backticks than it holds
fn code(s: &str) -> String {
    let longest = s.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    if longest > 0 { format!("{} {} {}", fence, s, fence) } else { format!("{}{}{}", fence, s, fence) }
}


/// [digest] md: the target, then by ticket - with --jira_prefix - the tables, and under
/// each table the scripts that change it and how. A script with nothing to show is not
/// counted, nor shown
fn render(entries: &[Entry], target: &str, jira: bool) -> String {
    let entries: Vec<&Entry> = entries.iter().filter(|e| e.has_any()).collect();
    let tables: BTreeSet<&String> = entries.iter().flat_map(|e| e.changes.keys()).filter(|k| !k.is_empty()).collect();
    let files = entries.iter().filter(|e| e.source.kind != 'B').count();
    let mut md = format!("## {}\n{}\n", esc(&tr!("digest.title", target = target)),
        tr!("digest.summary", tables = tables.len(), files = files));
    if entries.iter().any(|e| e.changes.values().any(|c| !c.uncounted.is_empty())) {
        md.push_str(&format!("\n{}\n", tr!("digest.counts")));
    }
    if jira {
        let mut all: Vec<&String> = entries.iter().flat_map(|e| e.tickets.iter()).collect::<BTreeSet<_>>().into_iter().collect();
        all.sort_by_key(|t| ticket_key(t));
        for t in all {
            md.push_str(&format!("\n### {}\n", t));
            group(&mut md, &entries.iter().copied().filter(|e| e.tickets.contains(t)).collect::<Vec<_>>());
        }
        let none: Vec<&Entry> = entries.iter().copied().filter(|e| e.tickets.is_empty()).collect();
        if !none.is_empty() {
            md.push_str(&format!("\n### {}\n", tr!("digest.no_ticket")));
            group(&mut md, &none);
        }
    } else {
        group(&mut md, &entries);
    }
    md
}

/// PROJ-12 after PROJ-2
fn ticket_key(t: &str) -> (String, u64) {
    match t.rsplit_once('-') {
        Some((p, n)) => (p.to_string(), n.parse().unwrap_or(0)),
        None => (t.to_string(), 0),
    }
}

/// the tables, other last: under each, its scripts' changes - and what was not read
fn group(md: &mut String, entries: &[&Entry]) {
    md.push('\n');
    let keys: BTreeSet<&String> = entries.iter().flat_map(|e| e.changes.keys()).filter(|k| !k.is_empty()).collect();
    for k in keys {
        md.push_str(&format!("- **{}**\n", esc(k)));
        for e in entries {
            if let Some(c) = e.changes.get(k) {
                md.push_str(&format!("  - {}: {}\n", label(e), describe(c)));
            }
        }
    }
    let mut other = vec![];
    for e in entries {
        let mut parts = vec![];
        if let Some(c) = e.changes.get(OTHER) {
            parts.push(describe(c));
        }
        if e.unread > 0 {
            parts.push(tr!("digest.not_parsed", count = e.unread));
        }
        if let Some(err) = &e.error {
            parts.push(esc(&tr!("digest.not_read", err = err)));
        }
        if !parts.is_empty() {
            other.push(format!("  - {}: {}\n", label(e), parts.join(" | ")));
        }
    }
    if !other.is_empty() {
        md.push_str(&format!("- **{}**\n", tr!("digest.other")));
        for o in other {
            md.push_str(&o);
        }
    }
}

/// the source: its file - which carries its version, 1_2 for 1.2 - or sge's history table; an
/// after_each script with the times it runs
fn label(e: &Entry) -> String {
    let s = &e.source;
    let mut l = if s.kind == 'B' {
        tr!("digest.history")
    } else if s.version.is_empty() || s.file.contains(&s.version) || s.file.contains(&s.version.replace('.', "_")) {
        code(&s.file)
    } else {
        format!("{} {}", code(&s.file), esc(&s.version))
    };
    if e.runs > 1 {
        l.push_str(&format!(" ×{}", e.runs));
    }
    l
}


/// ddl and its columns; dml and its rows - none that is 0 - and the statements whose rows
/// are not counted
fn describe(c: &Change) -> String {
    let mut parts = vec![];
    if !c.ddl.is_empty() {
        parts.push(format!("ddl {}", esc(&c.ddl.join("; "))));
    }
    let statements = |n: u64| format!("{} statement{}", n, if n == 1 { "" } else { "s" });
    let dml: Vec<String> = [("insert", c.inserts, Dml::Insert), ("update", c.updates, Dml::Update),
        ("delete", c.deletes, Dml::Delete), ("truncate", c.truncates, Dml::Truncate), ("merge", 0, Dml::Merge)]
        .iter().filter_map(|(w, n, d)| match (*n, c.uncounted.get(d).copied().unwrap_or(0)) {
            (0, 0) => None,
            (n, 0) => Some(format!("{} {}", w, n)),
            (0, s) => Some(format!("{} {}", w, statements(s))),
            (n, s) => Some(format!("{} {} + {}", w, n, statements(s))),
        }).collect();
    if !dml.is_empty() {
        parts.push(format!("dml {}", dml.join(", ")));
    }
    parts.join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(sql: &str) -> Analysis {
        Analysis::of(sql, "", "", &mut Owners::default())
    }

    fn entry(kind: char, version: &str, file: &str, sql: &str, tickets: Vec<&str>) -> Entry {
        let a = analyze(sql);
        Entry {
            source: Source { kind, version: version.into(), file: file.into() },
            tickets: tickets.into_iter().map(String::from).collect(),
            steps: a.steps,
            bare: a.bare,
            changes: BTreeMap::new(),
            unread: a.unread,
            error: None,
            runs: 1,
        }
    }

    /// SQL's changes, its steps taken with these answers - and its statements not read
    fn taken(sql: &str, answers: &Answers) -> (BTreeMap<String, Change>, usize) {
        let mut owners = Owners::default();
        let a = Analysis::of(sql, "", "", &mut owners);
        let mut e = vec![Entry { source: Source { kind: 'V', version: "1".into(), file: "V1.sql".into() }, tickets: vec![],
            steps: a.steps, bare: a.bare, changes: BTreeMap::new(), unread: a.unread, error: None, runs: 1 }];
        take_steps(&mut e, &owners, answers);
        let e = e.remove(0);
        (e.changes, e.unread)
    }

    /// SQL's changes with no database: every condition holds, no row is counted
    fn offline(sql: &str) -> BTreeMap<String, Change> {
        taken(sql, &Answers::new()).0
    }

    /// what SQL's steps ask the database
    fn asked(sql: &str) -> BTreeSet<Question> {
        let mut qs = BTreeSet::new();
        let steps = analyze(sql).steps;
        let mut r = HashMap::new();
        renames(&steps, &mut r);
        questions(&steps, &r, &mut qs);
        qs
    }

    fn md(entries: Vec<Entry>, target: &str, jira: bool) -> String {
        plan(Collector { target: target.into(), prefixes: if jira { vec!["PROJ".into()] } else { vec![] }, current: None,
            schema_tickets: vec![], owners: Owners::default(), entries }, &Answers::new())
    }

    /// statements not counted: one of each
    fn un(d: &[Dml]) -> BTreeMap<Dml, u64> {
        d.iter().map(|d| (*d, 1)).collect()
    }

    /// statements not counted, so many of each
    fn unn(d: &[(Dml, u64)]) -> BTreeMap<Dml, u64> {
        d.iter().copied().collect()
    }

    fn rel(n: &str) -> Question {
        Question::Exists(Object::Relation(n.into()), Lookup::Schema)
    }

    fn exists(o: Object) -> Question {
        Question::Exists(o, Lookup::Schema)
    }

    fn count(table: &str, sql: &str) -> Question {
        Question::Count(table.into(), Lookup::Schema, sql.into(), None)
    }

    fn tmp(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("sge-digest-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().to_string()
    }

    /// [digest] each statement on its table: columns of a CREATE and an ALTER, an index
    /// and a sequence where they belong, rows of an INSERT's VALUES; UPDATE and DELETE
    /// not counted with no database; INSERT ... SELECT not read; a SELECT is no change;
    /// a name without a schema in public, or in SET search_path's
    #[test]
    fn statements_are_changes_on_their_tables() {
        let (c, unread) = taken("CREATE TABLE app.Orders (id bigint primary key, user_id int, \"Total\" numeric);\n\
            CREATE INDEX orders_user ON app.orders (user_id);\n\
            CREATE SEQUENCE app.orders_seq OWNED BY app.orders.id;\n\
            ALTER TABLE app.orders ADD COLUMN email text, DROP COLUMN old;\n\
            ALTER TABLE app.orders RENAME COLUMN a TO b;\n\
            ALTER TABLE app.orders ADD CONSTRAINT chk CHECK (user_id > 0);\n\
            COMMENT ON COLUMN app.orders.email IS 'PROJ-1 is in a string';\n\
            INSERT INTO app.orders (id) VALUES (1), (2), (3);\n\
            INSERT INTO app.orders DEFAULT VALUES;\n\
            INSERT INTO app.orders SELECT * FROM app.old;\n\
            UPDATE app.orders SET user_id = 1 WHERE id = 2;\n\
            DELETE FROM app.orders WHERE id = 3;\n\
            SELECT setval('app.orders_id_seq', coalesce(max(id), 0)) + 1 FROM app.orders;\n\
            SELECT * FROM app.orders;\n\
            SAVEPOINT s;\n\
            CREATE SCHEMA IF NOT EXISTS app;\n\
            CREATE TABLE plain (a int);\n\
            SET search_path TO shop, public;\n\
            CREATE TABLE items (a int);\n", &Answers::new());
        assert_eq!(unread, 0);
        let o = &c["app.orders"];
        assert_eq!(o.ddl, vec![
            "create (id, user_id, Total)", "index orders_user (user_id)", "sequence app.orders_seq",
            "alter: add email, drop old", "alter: rename a to b", "alter: add constraint chk check", "comment email",
            "sequence app.orders_id_seq setval",
        ]);
        assert_eq!((o.inserts, o.updates, o.deletes, &o.uncounted), (4, 0, 0, &un(&[Dml::Update, Dml::Delete])));
        assert_eq!(c[OTHER].ddl, vec!["create schema app"]);
        assert_eq!(c["public.plain"].ddl, vec!["create (a)"]);
        assert_eq!(c["shop.items"].ddl, vec!["create (a)"]);
        assert!(!c.contains_key("app.old"));
        assert_eq!(c.len(), 4, "{:?}", c);
    }

    /// [plan] IF [NOT] EXISTS asked of the database - what the run made or dropped
    /// before counts first - and a DDL whose condition does not hold is no change; an
    /// UPDATE's, a DELETE's rows counted by their WHERE - with a FROM, a USING, a WITH -
    /// an INSERT's by its ON CONFLICT key; a table the database has not, not counted
    #[test]
    fn conditions_hold_and_rows_are_counted() {
        let sql = "CREATE TABLE IF NOT EXISTS app.t (id int);\n\
            CREATE TABLE app.n (id int);\n\
            CREATE TABLE IF NOT EXISTS app.n (id int);\n\
            DROP TABLE IF EXISTS app.gone;\n\
            ALTER TABLE app.t ADD COLUMN IF NOT EXISTS a int, ADD COLUMN b int;\n\
            ALTER TABLE IF EXISTS app.t DROP COLUMN IF EXISTS c;\n\
            CREATE INDEX IF NOT EXISTS t_a ON app.t (a);\n\
            DROP INDEX IF EXISTS app.t_old;\n\
            CREATE SCHEMA IF NOT EXISTS app;\n\
            CREATE EXTENSION IF NOT EXISTS pgcrypto;\n\
            DROP TRIGGER IF EXISTS trg ON app.t;\n\
            ALTER TABLE app.t DROP CONSTRAINT IF EXISTS chk;\n\
            UPDATE app.t SET a = 1 WHERE id > 2;\n\
            UPDATE app.t AS x SET a = s.a FROM app.s AS s WHERE s.id = x.id;\n\
            DELETE FROM app.t USING app.s WHERE app.s.id = app.t.id;\n\
            WITH k AS (SELECT id FROM app.s) DELETE FROM app.t WHERE id IN (SELECT id FROM k);\n\
            INSERT INTO app.t (id, a) VALUES (1, 10), (2, 20), (1, 30) ON CONFLICT (id) DO NOTHING;\n\
            INSERT INTO app.t (id, a) VALUES (5, now()) ON CONFLICT (id) DO UPDATE SET a = 0;\n\
            INSERT INTO app.t (id) VALUES (now()) ON CONFLICT (id) DO NOTHING;\n\
            UPDATE app.n SET id = 1;\n\
            INSERT INTO app.n (id) VALUES (1), (2), (1) ON CONFLICT (id) DO NOTHING;\n";
        let q = asked(sql);
        let counts = [
            ("SELECT count(*) FROM app.t WHERE id > 2", 3),
            ("SELECT count(*) FROM app.t AS x WHERE EXISTS (SELECT 1 FROM app.s AS s WHERE s.id = x.id)", 2),
            ("SELECT count(*) FROM app.t WHERE EXISTS (SELECT 1 FROM app.s WHERE app.s.id = app.t.id)", 1),
            ("WITH k AS (SELECT id FROM app.s) SELECT count(*) FROM app.t WHERE id IN (SELECT id FROM k)", 4),
            ("SELECT count(*) FROM app.t WHERE id IN (1, 2)", 1),
            ("SELECT count(*) FROM app.t WHERE id IN (5)", 1),
        ];
        for (c, _) in counts {
            assert!(q.contains(&count("app.t", c)), "{} in {:?}", c, q);
        }
        assert!(q.contains(&count("app.n", "SELECT count(*) FROM app.n")), "{:?}", q);
        let mut a: Answers = counts.iter().map(|(c, n)| (count("app.t", c), *n)).collect();
        for (o, n) in vec![(Object::Relation("app.t".into()), 1), (Object::Relation("app.n".into()), 0),
            (Object::Relation("app.gone".into()), 0), (Object::Column("app.t".into(), "a".into()), 1),
            (Object::Column("app.t".into(), "c".into()), 0), (Object::Relation("app.t_a".into()), 0),
            (Object::Relation("app.t_old".into()), 1), (Object::Schema("app".into()), 1),
            (Object::Extension("pgcrypto".into()), 0), (Object::Trigger("app.t".into(), "trg".into()), 1),
            (Object::Constraint("app.t".into(), "chk".into()), 0)] {
            a.insert(exists(o), n);
        }
        let (c, _) = taken(sql, &a);
        let t = &c["app.t"];
        assert_eq!(t.ddl, vec!["alter: add b", "index t_a (a)", "drop trigger trg"]);
        assert_eq!((t.inserts, t.updates, t.deletes, &t.uncounted), (1, 6, 5, &un(&[Dml::Insert])));
        assert_eq!(c["app.n"].ddl, vec!["create (id)"], "the run made it: IF NOT EXISTS holds not");
        assert_eq!(c["app.n"].uncounted, un(&[Dml::Update]));
        assert_eq!(c[OTHER].ddl, vec!["drop index t_old", "create extension pgcrypto"]);
        assert!(!c.contains_key("app.gone"));
        assert_eq!(c.len(), 3, "{:?}", c);
        assert_eq!(describe(t), "ddl alter: add b; index t_a (a); drop trigger trg | dml insert 1 + 1 statement, update 6, delete 5");
        // a key met twice is one row: a table the run makes, each new
        assert_eq!(c["app.n"].inserts, 2);
        // no database: every condition holds, no row counted
        let c = offline(sql);
        assert!(c.contains_key("app.gone"));
        // its CREATE TABLE IF NOT EXISTS taken to hold: a table the run makes - the keyed
        // inserts all new, the rest of its statements not counted
        assert_eq!(c["app.t"].uncounted, unn(&[(Dml::Insert, 1), (Dml::Update, 2), (Dml::Delete, 2)]));
    }

    /// [plan] a DO's IF: the branch its condition takes, asked of the database - inner
    /// IFs too - both when it is not answered; an IF with an ELSIF, its statements as
    /// they come
    #[test]
    fn a_do_blocks_if_takes_its_branch() {
        let sql = "DO $$\nBEGIN\n  IF NOT EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'x') THEN\n\
            \x20   CREATE TABLE app.x (id int);\n    IF (SELECT 1) = 1 THEN UPDATE app.y SET a = 1; END IF;\n  ELSE\n\
            \x20   DROP TABLE IF EXISTS app.z;\n  END IF;\n  UPDATE app.w SET a = 2;\nEND $$;\n\
            DO $$ BEGIN IF a THEN UPDATE app.e SET a = 1; ELSIF b THEN UPDATE app.f SET a = 1; END IF; END $$;\n";
        let outer = Question::Holds("NOT EXISTS (SELECT 1 FROM pg_tables WHERE tablename = 'x')".into(), None);
        let inner = Question::Holds("(SELECT 1) = 1".into(), None);
        let q = asked(sql);
        assert!(q.contains(&outer) && q.contains(&inner), "{:?}", q);
        let w = [(rel("app.w"), 1), (count("app.w", "SELECT count(*) FROM app.w"), 7), (rel("app.z"), 1)];
        let (c, unread) = taken(sql, &w.iter().cloned().chain(vec![(outer.clone(), 0)]).collect());
        assert_eq!(unread, 0);
        assert_eq!(c["app.z"].ddl, vec!["drop"]);
        assert_eq!(c["app.w"].updates, 7);
        assert!(!c.contains_key("app.x") && !c.contains_key("app.y"), "{:?}", c);
        let (c, _) = taken(sql, &w.iter().cloned().chain(vec![(outer, 1), (inner, 0)]).collect());
        assert_eq!(c["app.x"].ddl, vec!["create (id)"]);
        assert!(!c.contains_key("app.y") && !c.contains_key("app.z"), "{:?}", c);
        let c = offline(sql);
        for t in ["app.x", "app.y", "app.z", "app.w", "app.e", "app.f"] {
            assert!(c.contains_key(t), "{} in {:?}", t, c);
        }
    }

    /// [plan] a data file's rows: a table the database has not, all of them inserted;
    /// one it has, those whose key is there not - updated, when its rule updates - and
    /// those the clean deletes; not answered, not counted
    #[test]
    fn a_data_files_rows_are_counted_by_key() {
        let keyed = |update, deleted: Vec<&str>| vec![Entry { source: Source { kind: 'D', version: "1".into(), file: "t.csv".into() },
            tickets: vec![], steps: vec![Step::Keyed { table: "app.t".into(), find: Lookup::Schema, rows: 5, statements: 5,
                matched: vec!["m1".into(), "m2".into()], conflict: if update { Conflict::Update } else { Conflict::Nothing },
                deleted: deleted.into_iter().map(String::from).collect(), setval: true, path: None }],
            bare: Bare::new(), changes: BTreeMap::new(), unread: 0, error: None, runs: 1 }];
        let run = |mut e: Vec<Entry>, a: &Answers| {
            take_steps(&mut e, &Owners::default(), a);
            e.remove(0).changes.remove("app.t").unwrap_or_default()
        };
        let c = run(keyed(true, vec!["d"]), &vec![(rel("app.t"), 0)].into_iter().collect());
        assert_eq!((c.inserts, c.updates, c.deletes), (5, 0, 0));
        assert_eq!(c.ddl, vec!["sequence setval"]);
        let a: Answers = vec![(rel("app.t"), 1), (count("app.t", "m1"), 1), (count("app.t", "m2"), 2), (count("app.t", "d"), 4)]
            .into_iter().collect();
        let c = run(keyed(true, vec!["d"]), &a);
        assert_eq!((c.inserts, c.updates, c.deletes, c.uncounted.len()), (2, 3, 4, 0));
        let c = run(keyed(false, vec![]), &a);
        assert_eq!((c.inserts, c.updates, c.deletes), (2, 0, 0));
        // every key there, nothing updated: nothing loads, no sequence set
        let all: Answers = vec![(rel("app.t"), 1), (count("app.t", "m1"), 3), (count("app.t", "m2"), 2)].into_iter().collect();
        assert_eq!(run(keyed(false, vec![]), &all), Change::default());
        let c = run(keyed(true, vec!["d"]), &Answers::new());
        assert_eq!(c.uncounted, unn(&[(Dml::Insert, 5), (Dml::Update, 5), (Dml::Delete, 1)]));
        let c = run(keyed(true, vec!["gone"]), &vec![(rel("app.t"), 1)].into_iter().collect());
        assert_eq!(c.uncounted, unn(&[(Dml::Insert, 5), (Dml::Update, 5), (Dml::Delete, 1)]));
    }

    /// [plan] SET search_path: names in its first schema, and the database asked under
    /// it; TO DEFAULT, public and the database's own again
    #[test]
    fn a_search_path_is_asked_under() {
        let s = analyze("SET search_path TO shop, public;\nUPDATE items SET a = 1;\nSET search_path TO DEFAULT;\nDELETE FROM items;\n").steps;
        assert_eq!(s, vec![
            Step::Count { table: "shop.items".into(), find: Lookup::Any(Some("shop, public".into())), dml: Dml::Update,
                sql: Some("SELECT count(*) FROM items".into()), path: Some("shop, public".into()) },
            Step::Count { table: "public.items".into(), find: Lookup::Any(None), dml: Dml::Delete,
                sql: Some("SELECT count(*) FROM items".into()), path: None },
        ]);
        // a WITH that writes, a MERGE: not counted
        let s = analyze("WITH d AS (DELETE FROM a RETURNING id) UPDATE b SET x = 1 WHERE id IN (SELECT id FROM d);\n\
            MERGE INTO c USING d ON c.id = d.id WHEN MATCHED THEN DELETE;\n").steps;
        assert!(matches!(&s[1], Step::Count { table, dml: Dml::Update, sql: None, .. } if table == "public.b"), "{:?}", s);
        assert!(matches!(&s[2], Step::Count { dml: Dml::Merge, sql: None, .. }), "{:?}", s);
        assert_eq!(describe(&Change { uncounted: un(&[Dml::Merge]), ..Default::default() }), "dml merge 1 statement");
        assert_eq!(describe(&Change { updates: 4, uncounted: unn(&[(Dml::Update, 2)]), ..Default::default() }), "dml update 4 + 2 statements");
    }

    /// [digest] schema_guard's retry wraps its DDL in DO ... EXECUTE '...': read there;
    /// ALTER SEQUENCE ... OWNED BY, which sqlparser does not read, by its words; a DO
    /// with SQL of its own read from its first changing word
    #[test]
    fn do_blocks_and_alter_sequence_are_read() {
        let sql = "CREATE SEQUENCE IF NOT EXISTS app.t_id_seq;\n\
            DO\n$do$\nDECLARE\n   x CONSTANT text := '1000ms';\nBEGIN\n   FOR i IN 1..3 LOOP\n      BEGIN\n\
            EXECUTE 'ALTER TABLE app.t ADD COLUMN id integer default nextval(''app.t_id_seq''::regclass)';\n\
            EXIT;\n      EXCEPTION WHEN lock_not_available THEN NULL;\n      END;\n   END LOOP;\nEND\n$do$;\n\
            ALTER SEQUENCE app.t_id_seq OWNED BY app.t.id;\n\
            DO $$ BEGIN IF NOT EXISTS (SELECT 1) THEN ALTER TABLE app.u ADD COLUMN c int; END IF; END $$;\n\
            CREATE INDEX i ON ONLY app.v (c);\n";
        let (c, unread) = taken(sql, &Answers::new());
        assert_eq!(unread, 0);
        // the CREATE SEQUENCE came first: on its table, which the run tells
        assert_eq!(c["app.t"].ddl, vec!["sequence app.t_id_seq", "alter: add id", "sequence app.t_id_seq owned"]);
        assert_eq!(c["app.u"].ddl, vec!["alter: add c"]);
        assert_eq!(c["app.v"].ddl, vec!["index i"]);
        let (c, _) = taken(sql, &vec![(rel("app.t_id_seq"), 1), (Question::Holds("NOT EXISTS (SELECT 1)".into(), None), 0)]
            .into_iter().collect());
        assert_eq!(c["app.t"].ddl, vec!["alter: add id", "sequence app.t_id_seq owned"]);
        assert!(!c.contains_key("app.u"));
    }

    /// [digest] WITH ... DELETE counted, its INSERT ... SELECT not read; SELECT INTO
    /// changes a table; COPY is not read, nor its data; a \gset at a line's end costs no
    /// statement; one statement not read costs no other
    #[test]
    fn queries_that_change_copy_data_and_psql_endings() {
        let sql = "WITH m AS (DELETE FROM app.old RETURNING *) INSERT INTO app.new SELECT * FROM m;\n\
            SELECT * INTO app.copy FROM app.new;\n\
            CREATE TABLE app.a (x int);\n\
            COPY app.a FROM stdin;\n1\tit's\n\\.\n\
            SELECT count(*) FROM app.a \\gset\n\
            CREATE TABLE app.q (y int);\n\
            SELECT;\n\
            CREATE TABLE app.r (z int);\n\
            this is not sql;\n\
            CREATE TABLE app.s (w int);\n";
        assert!(asked(sql).contains(&count("app.old", "SELECT count(*) FROM app.old")));
        let (c, unread) = taken(sql, &Answers::new());
        assert_eq!(unread, 1);
        assert_eq!(c["app.old"].uncounted, un(&[Dml::Delete]));
        assert!(!c.contains_key("app.new"), "{:?}", c);
        assert_eq!(c["app.copy"].ddl, vec!["create (select into)"]);
        for t in ["app.q", "app.r", "app.s"] {
            assert!(c.contains_key(t), "{} in {:?}", t, c);
        }
        assert_eq!(c["app.a"], Change { ddl: vec!["create (x)".into()], ..Default::default() });
    }

    /// [digest] \i from psql's directory, \ir from the script's: read in place, their
    /// comments with the script's; one that is not there is counted
    #[test]
    fn includes_are_read() {
        let d = tmp("inc");
        fs::create_dir_all(format!("{}/sub", d)).unwrap();
        fs::write(format!("{}/common.sql", d), "-- OPS-4\nCREATE TABLE app.c (a int);\n").unwrap();
        fs::write(format!("{}/sub/near.sql", d), "CREATE TABLE app.n (a int);\n").unwrap();
        let text = "\\i common.sql\n\\ir near.sql\n\\i missing.sql\nINSERT INTO app.c VALUES (1);\n";
        let mut owners = Owners::default();
        let a = Analysis::of(text, &d, &format!("{}/sub", d), &mut owners);
        assert_eq!(a.unread, 1);
        assert_eq!(tickets(&a.comments, &prefixes("ops")), vec!["OPS-4"]);
        let mut e = vec![Entry { source: Source { kind: 'V', version: "1".into(), file: "V1.sql".into() }, tickets: vec![],
            steps: a.steps, bare: a.bare, changes: BTreeMap::new(), unread: 0, error: None, runs: 1 }];
        take_steps(&mut e, &owners, &Answers::new());
        assert!(e[0].changes.contains_key("app.n"), "{:?}", e[0].changes);
        assert_eq!(e[0].changes["app.c"].inserts, 1);
        let _ = fs::remove_dir_all(&d);
    }

    /// [jira] a project's tickets in comments, whole words, each once, by project and
    /// number - not in strings, not part of a longer word; YAML's quotes and block
    /// scalars are no comments
    #[test]
    fn tickets_are_found_in_comments() {
        let p = prefixes("proj, OPS-");
        assert_eq!(p, vec!["PROJ", "OPS"]);
        let sql = "-- PROJ-12 and PROJ-2, xPROJ-3, PROJ-4a\n/* OPS-7 /* PROJ-12 */ */ select 'PROJ-99 -- PROJ-98', $x$ -- PROJ-97 $x$, E'\\' -- PROJ-96';";
        assert_eq!(tickets(&sql_comments(sql), &p), vec!["PROJ-2", "PROJ-12", "OPS-7"]);
        let yaml = "tables: # OPS-1\n  - name: 'a # PROJ-5'\n    description: |\n      text # PROJ-8\n# PROJ-6\n";
        assert_eq!(tickets(&yaml_comments(yaml), &p), vec!["PROJ-6", "OPS-1"]);
        assert!(tickets("PROJ-1", &[]).is_empty());
        // COPY's data is no SQL: an apostrophe there opens no string
        let a = analyze("COPY t FROM stdin;\n1\tit's\n\\.\nselect 'a -- PROJ-55';\n-- PROJ-56\n");
        assert_eq!(tickets(&a.comments, &p), vec!["PROJ-56"]);
    }

    /// [digest] md: by ticket, the tables under it, each script's ddl and dml - none
    /// that is 0, ? not counted, said once - and what has no ticket last; a script with
    /// nothing in it is not counted; names are text, not marks
    #[test]
    fn the_digest_is_grouped_by_ticket_then_table() {
        let entries = vec![
            entry('S', "1", "S1__schema.yaml", "CREATE TABLE app.users (id int, name text);", vec!["PROJ-2"]),
            entry('V', "1_2", "V1_2__orders.sql", "CREATE TABLE app.orders (id int);\nCREATE INDEX o_u ON app.orders (id);\n\
                INSERT INTO app.users VALUES (1), (2);", vec!["PROJ-2", "PROJ-10"]),
            entry('R', "1", "R1__fix.sql", "UPDATE app.users SET name = 'x';\nwhat is this;", vec![]),
            entry('V', "1_3", "V1_3__look.sql", "SELECT 1;", vec!["PROJ-3"]),
        ];
        let got = md(entries.clone(), "app@db", true);
        let expected = format!("## {}\n{}\n\n{}\n\
            \n### PROJ-2\n\n\
            - **app.orders**\n  - `V1_2__orders.sql`: ddl create (id); index o_u (id)\n\
            - **app.users**\n  - `S1__schema.yaml`: ddl create (id, name)\n  - `V1_2__orders.sql`: dml insert 2\n\
            \n### PROJ-10\n\n\
            - **app.orders**\n  - `V1_2__orders.sql`: ddl create (id); index o_u (id)\n\
            - **app.users**\n  - `V1_2__orders.sql`: dml insert 2\n\
            \n### {}\n\n\
            - **app.users**\n  - `R1__fix.sql`: dml update 1 statement\n\
            - **{}**\n  - `R1__fix.sql`: {}\n",
            tr!("digest.title", target = "app@db"), tr!("digest.summary", tables = 2, files = 3), tr!("digest.counts"),
            tr!("digest.no_ticket"), tr!("digest.other"), tr!("digest.not_parsed", count = 1));
        assert_eq!(got, expected, "\n{}", got);
        // no --jira_prefix: the tables alone; nothing not counted, no word on it
        let got = md(entries[..1].to_vec(), "app@db", false);
        assert!(got.ends_with("\n\n- **app.users**\n  - `S1__schema.yaml`: ddl create (id, name)\n"), "{}", got);
        assert!(!got.contains("###") && !got.contains(&tr!("digest.counts")), "{}", got);
        // marks in names are text
        let got = md(vec![entry('V', "1", "V1__a`b.sql", "CREATE TABLE app.\"My*Table|x\" (\"_c_\" int);", vec![])], "x", false);
        assert!(got.contains("- **app.My\\*Table\\|x**\n  - `` V1__a`b.sql ``: ddl create (\\_c\\_)"), "{}", got);
    }

    /// [digest] sge's history table by its own name; a data file's version after it
    /// when its name has none; after_each with the times it runs
    #[test]
    fn a_source_is_its_file() {
        let e = |kind, version: &str, file: &str, runs| {
            let mut e = entry(kind, version, file, "", vec![]);
            e.runs = runs;
            label(&e)
        };
        assert_eq!(e('V', "1_2", "V1_2__a.sql", 1), "`V1_2__a.sql`");
        assert_eq!(e('V', "1.2", "V1_2__a.sql", 1), "`V1_2__a.sql`");
        assert_eq!(e('D', "3", "data/users.csv", 1), "`data/users.csv` 3");
        assert_eq!(e('B', "", "baseline", 1), tr!("digest.history"));
        assert_eq!(e(' ', "after_each", "after_each.sql", 3), "`after_each.sql` ×3");
        assert_eq!(path("db", "V1.sql"), "db/V1.sql");
        assert_eq!(path("db/", "V1.sql"), "db/V1.sql");
        assert_eq!(path("", "V1.sql"), "V1.sql");
        assert_eq!(path("db", "/abs/V1.sql"), "/abs/V1.sql");
    }

    /// [digest] a run's digest: a schema file's and sge's history table's changes as
    /// schema_guard's dry run hands them over, a data file's rows - with its schema file's
    /// tickets - a script's read; after_each counted
    #[tokio::test]
    async fn a_run_writes_its_digest() {
        let d = tmp("run");
        fs::write(format!("{}/S1__schema.yaml", d), "# PROJ-1\ntables: []\n").unwrap();
        fs::write(format!("{}/V1_1__seq.sql", d), "-- PROJ-2\nCREATE SEQUENCE app.n_seq;\n").unwrap();
        fs::write(format!("{}/after_each.sql", d), "UPDATE app.t SET a = 1;\n").unwrap();
        let cfg = ConfigMap::of(&[("jira_prefix", "proj"), ("db_name", "app"), ("db_host", "db")]);
        let c = scope(async {
            start(&cfg);
            source('B', "", "baseline", None);
            changes(Some("public.sge_history"), Some(&Changes { create: true, columns: vec!["id".into()], ..Default::default() }));
            source('S', "1", "S1__schema.yaml", Some(&path(&d, "S1__schema.yaml")));
            changes(Some("app.t"), Some(&Changes { create: true, columns: vec!["a".into(), "n".into()],
                indexes: vec!["t_a".into()], rows: 2, ..Default::default() }));
            // a table with no changes, and SQL of no table: nothing
            changes(Some("app.same"), None);
            changes(None, Some(&Changes { comment: true, ..Default::default() }));
            changes(Some("app.u"), Some(&Changes { columns: vec!["email".into()], altered: vec!["total".into()],
                foreign_keys: vec!["user_id".into()], grants: vec!["report".into()], ..Default::default() }));
            source('D', "1", "data/t.csv", None);
            data("App.T", 2, vec![], false, vec![], true);
            script('V', "1_1", "V1_1__seq.sql", &d);
            script(' ', "after_each", "after_each.sql", &d);
            script(' ', "after_each", "after_each.sql", &d);
            take().unwrap()
        }).await;
        // app.t is the run's: the database has it not
        let md = plan(c, &vec![(rel("app.t"), 0)].into_iter().collect());
        assert!(md.contains(concat!("\n### PROJ-1\n\n- **app.t**\n",
            "  - `S1__schema.yaml`: ddl create (a, n); index t_a | dml insert 2\n",
            "  - `data/t.csv` 1: ddl sequence setval | dml insert 2\n",
            "- **app.u**\n  - `S1__schema.yaml`: ddl alter: add email, alter total; foreign key user_id; grant report\n")), "{}", md);
        assert!(md.contains("\n### PROJ-2\n\n- **other**\n  - `V1_1__seq.sql`: ddl sequence app.n_seq\n"), "{}", md);
        assert!(md.contains("- **public.sge_history**\n"), "{}", md);
        assert!(md.contains("  - `after_each.sql` ×2: dml update 1 statement\n"), "{}", md);
        assert!(md.starts_with(&format!("## {}\n", tr!("digest.title", target = "app@db"))), "{}", md);
        // no digest outside a plan's scope
        data("app.x", 1, vec![], false, vec![], false);
        assert!(take().is_none());
        let _ = fs::remove_dir_all(&d);
    }

    /// [digest] views, triggers, policies, drops, comments, grants, TRUNCATE and MERGE on
    /// their tables - an index or a sequence on the table the run tells - the rest under
    /// other; a long column list counted
    #[test]
    fn more_statements_on_their_tables() {
        let wide: Vec<String> = (1..=14).map(|i| format!("c{} int", i)).collect();
        let (c, unread) = taken(&format!("CREATE TABLE app.wide ({});\n\
            CREATE TABLE app.s (id serial, n int);\n\
            CREATE VIEW app.v AS SELECT 1;\n\
            CREATE MATERIALIZED VIEW app.mv AS SELECT 1;\n\
            CREATE TRIGGER trg BEFORE INSERT ON app.s FOR EACH ROW EXECUTE FUNCTION f();\n\
            DROP TRIGGER trg ON app.s;\n\
            CREATE POLICY p ON app.s USING (true);\n\
            CREATE INDEX s_n ON app.s (n);\n\
            ALTER INDEX app.s_n RENAME TO s_m;\n\
            DROP INDEX app.s_n;\n\
            DROP INDEX app.lone;\n\
            DROP SEQUENCE app.s_id_seq;\n\
            DROP TABLE app.old;\n\
            DROP SCHEMA x;\n\
            COMMENT ON TABLE app.s IS 'x';\n\
            COMMENT ON COLUMN s.n IS 'x';\n\
            COMMENT ON INDEX app.s_n IS 'x';\n\
            COMMENT ON SCHEMA app IS 'x';\n\
            GRANT SELECT ON app.s TO r;\n\
            GRANT USAGE ON SCHEMA app TO r;\n\
            REVOKE SELECT ON app.s FROM r;\n\
            TRUNCATE app.s;\n\
            MERGE INTO app.s USING app.src ON app.s.id = app.src.id WHEN MATCHED THEN UPDATE SET n = 1;\n\
            SELECT 1 UNION SELECT 2;\n\
            (SELECT 1);\n\
            ALTER TABLE app.s ALTER COLUMN n SET DEFAULT nextval('n_seq');\n\
            ALTER TABLE app.s RENAME CONSTRAINT a TO b;\n\
            ALTER TABLE app.s DROP CONSTRAINT c;\n\
            ALTER TABLE app.s OWNER TO someone;\n\
            CREATE EXTENSION hstore;\n\
            DROP EXTENSION IF EXISTS hstore, ltree;\n\
            CREATE SEQUENCE public.n_seq;\n", wide.join(", ")), &Answers::new());
        assert_eq!(unread, 0);
        assert_eq!(c["app.wide"].ddl, vec!["create (c1, c2, c3, c4, c5, c6, c7, c8, c9, c10, c11, c12, +2)"]);
        assert_eq!(c["app.v"].ddl, vec!["create view"]);
        assert_eq!(c["app.mv"].ddl, vec!["create materialized view"]);
        assert_eq!(c["app.old"].ddl, vec!["drop"]);
        assert_eq!(c["public.s"].ddl, vec!["comment n"]);
        let s = &c["app.s"];
        assert_eq!(s.ddl, vec!["create (id, n)", "trigger trg", "drop trigger trg", "policy p", "index s_n (n)", "alter index s_n",
            "drop index s_n", "drop sequence app.s_id_seq", "comment", "comment index s_n", "grant", "revoke",
            "alter: alter n", "alter: rename constraint a to b", "alter: drop constraint c", "alter: owner to someone",
            "sequence public.n_seq"], "{:?}", s.ddl);
        assert_eq!(s.uncounted, un(&[Dml::Truncate, Dml::Merge]));
        // a lone index under other; the sequence made after the default that takes from it, on that table
        assert_eq!(c[OTHER].ddl, vec!["drop index lone", "drop schema x", "comment on schema app", "grant on schema app",
            "create extension hstore", "drop extension hstore", "drop extension ltree"], "{:?}", c[OTHER]);
        assert!(!c.contains_key("public.n_seq"));
    }

    /// [digest] text the tokenizer cannot read, read by its lines' semicolons - one bad
    /// statement counted; DO in its other forms; ALTER SEQUENCE without OWNED BY, and a
    /// quoted name
    #[test]
    fn unreadable_text_and_do_forms() {
        let (c, unread) = taken("CREATE TABLE app.a (x int);\nSELECT 'unterminated;\nCREATE TABLE app.b (y int);\n", &Answers::new());
        assert!(c.contains_key("app.a") && c.contains_key("app.b"), "{:?}", c);
        assert_eq!(unread, 1);
        let (c, unread) = taken("DO 'BEGIN EXECUTE ''CREATE TABLE app.c (z int)''; END';\n\
            DO;\n\
            DO $$ BEGIN EXECUTE $q$CREATE TABLE app.d (w int)$q$; END $$;\n\
            DO $$ BEGIN PERFORM 1; END $$;\n\
            DO $$ BEGIN IF x THEN UPDATE app.e SET a = 1; END $$;\n\
            ALTER SEQUENCE app.x RESTART;\n\
            CREATE INDEX \"Idx\" ON ONLY app.v (c);\n\
            CREATE INDEX ON ONLY app.w (c);\n", &Answers::new());
        assert_eq!(unread, 2);
        assert!(c.contains_key("app.c") && c.contains_key("app.d") && c.contains_key("app.e"), "{:?}", c);
        assert_eq!(c["app.v"].ddl, vec!["index Idx"]);
        assert_eq!(c["app.w"].ddl, vec!["index"]);
        assert_eq!(c[OTHER].ddl, vec!["alter sequence app.x"]);
        // \i with no file, and one too deep, are counted
        let mut owners = Owners::default();
        let mut r = Reader::new(&mut owners, "");
        r.depth = DEPTH;
        r.read("\\i\n\\i x.sql\n", "");
        assert_eq!(r.a.unread, 2);
    }

    /// [digest] the collector: a script not read is said; a source met again with new
    /// tickets has them; the changes schema_guard hands over - primary key, owner,
    /// comment, triggers - as they are
    #[tokio::test]
    async fn the_collector_keeps_what_it_is_handed() {
        let d = tmp("coll");
        fs::write(format!("{}/a.yaml", d), "# PROJ-1\n").unwrap();
        fs::write(format!("{}/b.yaml", d), "# PROJ-2\n").unwrap();
        let c = scope(async {
            start(&ConfigMap::of(&[("jira_prefix", "PROJ"), ("db_name", "app"), ("db_host", "db")]));
            script('V', "9", "V9__gone.sql", &d);
            source('S', "1", "S1.yaml", Some(&path(&d, "a.yaml")));
            changes(Some("app.t"), Some(&Changes { primary_key: true, owner: true, comment: true,
                triggers: vec!["trg".into()], ..Default::default() }));
            source('S', "1", "S1.yaml", Some(&path(&d, "b.yaml")));
            changes(Some("app.t"), Some(&Changes { columns: vec!["c".into()], ..Default::default() }));
            take().unwrap()
        }).await;
        let md = finish(c, None).await;
        assert!(md.starts_with(&format!("## {}\n", tr!("digest.title", target = "app@db"))), "{}", md);
        assert!(md.contains("\n### PROJ-1\n\n- **app.t**\n  - `S1.yaml`: ddl alter: primary key, owner; comment; trigger trg; alter: add c\n"), "{}", md);
        assert!(md.contains("\n### PROJ-2\n"), "{}", md);
        assert!(md.contains("  - `V9__gone.sql`: "), "{}", md);
        assert_eq!(ticket_key("plain"), ("plain".to_string(), 0));
        // a $1 parameter is no tag, a prefix not followed by - no ticket
        assert_eq!(tickets(&sql_comments("select $1; -- PROJECT-1 PROJ-3"), &prefixes("PROJ")), vec!["PROJ-3"]);
        let _ = fs::remove_dir_all(&d);
    }

    /// [plan] nothing is collected before a start, nor outside a plan's scope
    #[tokio::test]
    async fn nothing_is_collected_without_a_start() {
        let none = scope(async {
            data("app.t", 1, vec![], false, vec![], false);
            take()
        }).await;
        assert!(none.is_none());
        assert!(take().is_none());
    }

    /// [plan] review 1 - fixed: a CASE in an IF - its THEN, ELSE and END - is the CASE's,
    /// an expression's or plpgsql's; a statement with words left over is not read, not
    /// taken for its first part
    #[test]
    fn a_case_in_an_if_is_the_cases() {
        let s = analyze("DO $$ BEGIN IF (SELECT 1) = 1 THEN UPDATE app.t SET a = CASE WHEN b THEN 1 ELSE 2 END WHERE id = 5;\n\
            ELSE DELETE FROM app.u; END IF; END $$;\n\
            DO $$ BEGIN IF x THEN CASE y WHEN 1 THEN UPDATE app.v SET a = 1; ELSE UPDATE app.w SET a = 1; END CASE; END IF; END $$;\n\
            DO $$ BEGIN IF CASE WHEN a THEN true ELSE false END THEN UPDATE app.z SET a = 1; END IF; END $$;\n").steps;
        let tables = |steps: &[Step]| steps.iter().map(|s| match s {
            Step::Count { table, sql, .. } => format!("{} {}", table, sql.clone().unwrap_or_default()),
            o => format!("{:?}", o),
        }).collect::<Vec<_>>();
        match &s[0] {
            Step::When { then, otherwise, .. } => {
                assert_eq!(tables(then), vec!["app.t SELECT count(*) FROM app.t WHERE id = 5"]);
                assert_eq!(tables(otherwise), vec!["app.u SELECT count(*) FROM app.u"]);
            }
            o => panic!("{:?}", o),
        }
        match &s[1] {
            Step::When { then, otherwise, .. } => {
                assert_eq!(tables(then), vec!["app.v SELECT count(*) FROM app.v", "app.w SELECT count(*) FROM app.w"]);
                assert!(otherwise.is_empty(), "{:?}", otherwise);
            }
            o => panic!("{:?}", o),
        }
        assert!(matches!(&s[2], Step::When { cond, .. } if cond == "CASE WHEN a THEN true ELSE false END"), "{:?}", s[2]);
        // words left over: not read
        assert!(parse(Tokenizer::new(&PostgreSqlDialect {}, "UPDATE app.t SET a = CASE WHEN b THEN 1").tokenize().unwrap()).is_none());
        let (c, _) = taken("DELETE FROM ONLY app.p WHERE id = 1;\n", &Answers::new());
        assert!(!c.contains_key("public.only"), "{:?}", c);
    }

    /// [plan] review 2 - fixed: a DO whose statement changes and cannot be read - EXECUTE of
    /// SQL of its own making - is said, in an IF too
    #[test]
    fn a_do_not_read_is_said() {
        let (c, unread) = taken("DO $$ BEGIN IF NOT EXISTS (SELECT 1) THEN EXECUTE format('CREATE TABLE %I (a int)', 'x'); END IF; END $$;\n\
            DO $$ BEGIN IF true THEN UPDATE app.t SET; END IF; END $$;\n\
            DO $$ DECLARE comment text; BEGIN PERFORM 1; END $$;\n", &Answers::new());
        assert!(c.is_empty(), "{:?}", c);
        assert_eq!(unread, 3);
    }

    /// [plan] review 3 - fixed: a search_path of a name in letters of more than a byte
    #[test]
    fn a_search_path_in_any_letters() {
        let (c, _) = taken("SET search_path TO схема, public;\nCREATE TABLE t (a int);\nSET search_path = \"Ünï\";\nCREATE TABLE u (a int);\n",
            &Answers::new());
        assert!(c.contains_key("схема.t") && c.contains_key("Ünï.u"), "{:?}", c);
    }

    /// [plan] review 5, 7, 13 - fixed: what the run made or dropped anew has nothing the
    /// database has - a table its indexes, columns, rows; a schema its tables - and its
    /// constraints, renames, a quoted index name are followed
    #[test]
    fn the_run_makes_anew() {
        let sql = "DROP TABLE IF EXISTS app.x;\nCREATE TABLE app.x (id int primary key, a int);\n\
            CREATE INDEX IF NOT EXISTS x_a ON app.x (a);\nALTER TABLE app.x ADD COLUMN IF NOT EXISTS b int;\n\
            INSERT INTO app.x (id) VALUES (1), (2) ON CONFLICT (id) DO NOTHING;\nUPDATE app.x SET a = 1;\n\
            ALTER TABLE app.t ADD CONSTRAINT chk CHECK (a > 0);\nALTER TABLE app.t DROP CONSTRAINT IF EXISTS chk;\n\
            ALTER TABLE app.t RENAME CONSTRAINT k1 TO k2;\nALTER TABLE app.t DROP CONSTRAINT IF EXISTS k1;\n\
            ALTER TABLE app.r RENAME TO r_old;\nCREATE TABLE IF NOT EXISTS app.r (id int);\n\
            DROP SCHEMA s CASCADE;\nCREATE SCHEMA s;\nCREATE TABLE IF NOT EXISTS s.t (id int);\n\
            ALTER INDEX app.i1 RENAME TO i2;\nCREATE INDEX IF NOT EXISTS i1 ON app.q (a);\n\
            CREATE INDEX IF NOT EXISTS \"MyIdx\" ON app.q (a);\n";
        let q = asked(sql);
        assert!(q.contains(&rel("app.MyIdx")), "{:?}", q);
        let a: Answers = vec![(rel("app.x"), 1), (rel("app.x_a"), 1), (exists(Object::Column("app.x".into(), "b".into())), 1),
            (count("app.x", "SELECT count(*) FROM app.x WHERE id IN (1, 2)"), 2), (count("app.x", "SELECT count(*) FROM app.x"), 5),
            (exists(Object::Constraint("app.t".into(), "chk".into())), 0), (exists(Object::Constraint("app.t".into(), "k1".into())), 1),
            (rel("app.r"), 1), (rel("s.t"), 1), (rel("app.i1"), 1), (rel("app.MyIdx"), 1)].into_iter().collect();
        let (c, _) = taken(sql, &a);
        let x = &c["app.x"];
        assert_eq!(x.ddl, vec!["drop", "create (id, a)", "index x_a (a)", "alter: add b"]);
        assert_eq!((x.inserts, x.updates, &x.uncounted), (2, 0, &un(&[Dml::Update])));
        assert_eq!(c["app.t"].ddl, vec!["alter: add constraint chk check", "alter: drop constraint chk", "alter: rename constraint k1 to k2"]);
        assert_eq!(c["app.r"].ddl, vec!["alter: rename to r_old", "create (id)"]);
        assert_eq!(c["s.t"].ddl, vec!["create (id)"]);
        // renamed away: IF NOT EXISTS holds; the quoted name the database has
        assert!(c["app.q"].ddl.contains(&"index i1 (a)".to_string()) && !c["app.q"].ddl.iter().any(|d| d.contains("MyIdx")), "{:?}", c["app.q"]);
    }

    /// [plan] review 6, 16 - fixed: a SELECT of no table with a WHERE inserts what it lets
    /// through, counted; ON CONFLICT DO UPDATE ... WHERE updates some, not counted
    #[test]
    fn inserts_on_a_condition_are_counted() {
        let sql = "INSERT INTO app.t (id) SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM app.t WHERE id = 1);\n\
            INSERT INTO app.t (id) SELECT 2;\n\
            INSERT INTO app.t (id, a) VALUES (3, 1) ON CONFLICT (id) DO UPDATE SET a = 2 WHERE app.t.a < 2;\n";
        let seed = "SELECT count(*) FROM (SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM app.t WHERE id = 1)) AS s";
        assert!(asked(sql).contains(&count("app.t", seed)), "{:?}", asked(sql));
        let a: Answers = vec![(rel("app.t"), 1), (count("app.t", seed), 0), (count("app.t", "SELECT count(*) FROM app.t WHERE id IN (3)"), 1)]
            .into_iter().collect();
        let (c, _) = taken(sql, &a);
        assert_eq!((c["app.t"].inserts, c["app.t"].updates, &c["app.t"].uncounted), (1, 0, &un(&[Dml::Update])));
    }

    /// [plan] review 9 - fixed: an IF's condition sent as SQL: its strings quoted again, its
    /// comments left out
    #[test]
    fn a_condition_is_sql_again() {
        let s = analyze("DO $$ BEGIN IF n = 'O''Brien' AND m = E'a\\'b' -- why\n /* and */ THEN UPDATE app.t SET a = 1; END IF; END $$;\n").steps;
        assert!(matches!(&s[0], Step::When { cond, .. } if cond == "n = 'O''Brien' AND m = E'a\\'b'"), "{:?}", s[0]);
    }

    /// [plan] review 12 - fixed: schema_guard's names, unquoted, as Postgres has them
    #[tokio::test]
    async fn schema_guards_names_in_lower_case() {
        let c = scope(async {
            start(&ConfigMap::of(&[]));
            source('S', "1", "S1.yaml", None);
            changes(Some("App.Users"), Some(&Changes { create: true, columns: vec!["Id".into()], indexes: vec!["Users_Id".into()],
                ..Default::default() }));
            take().unwrap()
        }).await;
        let mut e = c.entries;
        assert!(e[0].steps.contains(&Step::Made(vec![(Object::Relation("app.users".into()), true),
            (Object::Column("app.users".into(), "id".into()), true), (Object::Relation("app.users_id".into()), true)])), "{:?}", e[0].steps);
        take_steps(&mut e, &c.owners, &Answers::new());
        assert!(e[0].changes.contains_key("app.users"), "{:?}", e[0].changes);
    }

    /// [plan] review 14, 16 - fixed: a name written without its schema found under the
    /// search_path - in any of its schemas, a CREATE's in its first - and told in the one
    /// the database has it in; RESET search_path, the database's own again
    #[test]
    fn a_name_without_a_schema_is_found_under_the_search_path() {
        let sql = "SET search_path TO shop, public;\nDROP TABLE IF EXISTS items;\nCREATE TABLE IF NOT EXISTS carts (id int);\n\
            RESET search_path;\nDROP TABLE IF EXISTS orders;\n";
        let path = Some("shop, public".to_string());
        let q = asked(sql);
        for x in [Question::Exists(Object::Relation("shop.items".into()), Lookup::Any(path.clone())),
            Question::Exists(Object::Relation("shop.carts".into()), Lookup::First(path.clone())),
            Question::Exists(Object::Relation("public.orders".into()), Lookup::Any(None))] {
            assert!(q.contains(&x), "{:?} in {:?}", x, q);
        }
        let a = analyze(sql);
        assert!(a.bare.contains(&("shop.items".into(), "items".into(), "shop, public".into())), "{:?}", a.bare);
        let answers: Answers = vec![(Question::Exists(Object::Relation("shop.items".into()), Lookup::Any(path.clone())), 1),
            (Question::Found("items".into(), "shop, public".into()), 2),
            (Question::Exists(Object::Relation("public.orders".into()), Lookup::Any(None)), 0)].into_iter().collect();
        let (c, _) = taken(sql, &answers);
        assert_eq!(c["public.items"].ddl, vec!["drop"]);
        assert_eq!(c["shop.carts"].ddl, vec!["create (id)"]);
        assert!(!c.contains_key("shop.items") && !c.contains_key("public.orders"), "{:?}", c);
        assert_eq!(schemas("\"$user\", Shop, \"Mixed\", public"), vec!["shop", "Mixed", "public"]);
    }

    /// [plan] review 15 - fixed: a WITH's DELETE counted with the WITHs before it
    #[test]
    fn a_withs_statement_has_the_withs_before_it() {
        let s = analyze("WITH a AS (SELECT id FROM app.s), d AS (DELETE FROM app.t WHERE id IN (SELECT id FROM a) RETURNING id) \
            SELECT * FROM d;\n").steps;
        assert_eq!(s, vec![Step::Count { table: "app.t".into(), find: Lookup::Schema, dml: Dml::Delete,
            sql: Some("WITH a AS (SELECT id FROM app.s) SELECT count(*) FROM app.t WHERE id IN (SELECT id FROM a)".into()), path: None }]);
    }

    /// [plan] review 16 - fixed: DROP FUNCTION, TYPE, POLICY ... IF EXISTS asked too
    #[test]
    fn functions_types_and_policies_are_asked() {
        let sql = "DROP FUNCTION IF EXISTS app.f(int), app.g();\nDROP TYPE IF EXISTS app.ty;\nDROP POLICY IF EXISTS p ON app.t;\n\
            CREATE POLICY p ON app.t USING (true);\nDROP POLICY IF EXISTS p ON app.t;\n\
            CREATE FUNCTION app.h() RETURNS int LANGUAGE sql AS 'SELECT 1';\nDROP FUNCTION IF EXISTS app.h();\n\
            CREATE TYPE app.ty2 AS (a int);\nDROP TYPE IF EXISTS app.ty2;\n";
        let a: Answers = vec![(exists(Object::Function("app.f".into())), 0), (exists(Object::Function("app.g".into())), 1),
            (exists(Object::Type("app.ty".into())), 1), (exists(Object::Policy("app.t".into(), "p".into())), 0)].into_iter().collect();
        let (c, _) = taken(sql, &a);
        assert_eq!(c[OTHER].ddl, vec!["drop function app.g", "drop type app.ty", "create function app.h", "drop function app.h",
            "create type app.ty2", "drop type app.ty2"]);
        assert_eq!(c["app.t"].ddl, vec!["policy p", "drop policy p"]);
    }

    /// [plan] re-check 1 - fixed: a CASE arm's BEGIN ... END block is the block's, its END
    /// no CASE's
    #[test]
    fn a_block_in_a_case_in_an_if() {
        let s = analyze("DO $$ BEGIN IF c THEN CASE v WHEN 1 THEN BEGIN UPDATE app.e SET a = 1; END; ELSE UPDATE app.f SET a = 1; \
            END CASE; ELSE UPDATE app.g SET a = 1; END IF; END $$;\n").steps;
        match &s[0] {
            Step::When { then, otherwise, .. } => {
                let t = |steps: &[Step]| steps.iter().filter_map(|s| match s { Step::Count { table, .. } => Some(table.clone()), _ => None })
                    .collect::<Vec<_>>();
                assert_eq!(t(then), vec!["app.e", "app.f"]);
                assert_eq!(t(otherwise), vec!["app.g"]);
            }
            o => panic!("{:?}", o),
        }
    }

    /// [plan] re-check 7 - fixed: a table the run renames has what it had by its old name -
    /// its columns asked by it - and its rows, named by the new name, are not counted
    #[test]
    fn a_renamed_table_keeps_what_it_had() {
        let sql = "ALTER TABLE app.t RENAME TO t2;\nALTER TABLE app.t2 DROP COLUMN IF EXISTS c;\n\
            ALTER TABLE app.t2 ADD COLUMN IF NOT EXISTS d int;\nINSERT INTO app.t2 (id) VALUES (1) ON CONFLICT (id) DO NOTHING;\n\
            UPDATE app.t2 SET d = 1;\n";
        let c_t = exists(Object::Column("app.t".into(), "c".into()));
        let d_t = exists(Object::Column("app.t".into(), "d".into()));
        let q = asked(sql);
        assert!(q.contains(&c_t) && q.contains(&d_t), "{:?}", q);
        let (c, _) = taken(sql, &vec![(c_t, 1), (d_t, 1), (rel("app.t2"), 0)].into_iter().collect());
        assert_eq!(c["app.t"].ddl, vec!["alter: rename to t2"]);
        let t2 = &c["app.t2"];
        assert_eq!(t2.ddl, vec!["alter: drop c"]);
        assert_eq!((t2.inserts, &t2.uncounted), (0, &un(&[Dml::Insert, Dml::Update])));
    }

    /// [plan] re-check new 2 - fixed: what the run creates in the search_path's first
    /// schema is told there, though a later one has its name
    #[test]
    fn a_create_is_in_the_first_schema() {
        let sql = "SET search_path TO shop, public;\nCREATE TABLE IF NOT EXISTS items (a int);\nINSERT INTO items VALUES (1);\n";
        let path = Some("shop, public".to_string());
        let a: Answers = vec![(Question::Exists(Object::Relation("shop.items".into()), Lookup::First(path)), 0),
            (Question::Found("items".into(), "shop, public".into()), 2)].into_iter().collect();
        let (c, _) = taken(sql, &a);
        assert_eq!(c["shop.items"], Change { ddl: vec!["create (a)".into()], inserts: 1, ..Default::default() });
        assert!(!c.contains_key("public.items"), "{:?}", c);
    }

    /// [plan] re-check new 3 - fixed: a DDL's options sqlparser does not read are words left
    /// over that tell the plan nothing: the DDL is read
    #[test]
    fn a_ddls_options_left_over() {
        let (c, unread) = taken("CREATE SEQUENCE app.s START WITH 1 INCREMENT BY 1 NO MINVALUE NO MAXVALUE CACHE 1 OWNED BY app.t.id;\n\
            CREATE MATERIALIZED VIEW app.mv AS SELECT 1 WITH NO DATA;\n", &Answers::new());
        assert_eq!(unread, 0, "{:?}", c);
        assert!(c.values().any(|x| x.ddl.contains(&"sequence app.s".to_string())), "{:?}", c);
        assert_eq!(c["app.mv"].ddl, vec!["create materialized view"]);
    }

    /// [plan] re-check new 1 - fixed: a question holds no lock once answered, nor leaves a
    /// transaction open; one that turns read only off changes nothing for the next
    #[tokio::test]
    async fn a_question_holds_no_lock() {
        let Some(pg) = crate::tests::Pg::start("lock") else { return };
        let host = pg.dir.to_string_lossy().to_string();
        let (db, conn) = tokio_postgres::connect(&format!("host={} port={} user=postgres dbname=postgres", host, pg.port),
            tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(conn);
        db.batch_execute("CREATE TABLE public.t (id int); INSERT INTO public.t VALUES (1); CREATE SEQUENCE public.s;").await.unwrap();
        let qs: BTreeSet<Question> = vec![rel("public.t"), count("public.t", "SELECT count(*) FROM public.t"),
            count("public.t", "SELECT count(*) FROM public.t WHERE set_config('default_transaction_read_only', 'off', false) = 'off'"),
            count("public.t", "SELECT count(*) FROM public.t WHERE nextval('public.s') > 0")].into_iter().collect();
        let a = ask(&db, qs).await;
        assert_eq!(a.get(&count("public.t", "SELECT count(*) FROM public.t")), Some(&1));
        assert!(!a.contains_key(&count("public.t", "SELECT count(*) FROM public.t WHERE nextval('public.s') > 0")), "{:?}", a);
        let locks: i64 = db.query_one("SELECT count(*) FROM pg_locks WHERE pid = pg_backend_pid() AND relation = 'public.t'::regclass",
            &[]).await.unwrap().get(0);
        assert_eq!(locks, 0);
        let called: bool = db.query_one("SELECT is_called FROM public.s", &[]).await.unwrap().get(0);
        assert!(!called);
        let ro: String = db.query_one("SHOW default_transaction_read_only", &[]).await.unwrap().get(0);
        assert_eq!(ro, "off", "the session as it was");
    }

    /// [plan] re-check 3 - fixed: a table renamed keeps what the run made for it; a table
    /// made again under the old name lends it nothing; a new name dropped and made again
    /// is a new table
    #[test]
    fn a_rename_and_its_names_made_again() {
        let sql = "ALTER TABLE app.t ADD COLUMN x int;\nALTER TABLE app.t RENAME TO t_old;\nCREATE TABLE app.t (id int, b int);\n\
            ALTER TABLE app.t_old ADD COLUMN IF NOT EXISTS b int;\nALTER TABLE app.t_old DROP COLUMN IF EXISTS x;\n\
            ALTER TABLE app.t DROP COLUMN IF EXISTS x;\n\
            ALTER TABLE app.u RENAME TO u2;\nDROP TABLE app.u2;\nCREATE TABLE app.u2 (id int);\n\
            ALTER TABLE app.u2 ADD COLUMN IF NOT EXISTS c int;\nINSERT INTO app.u2 (id) VALUES (1), (2);\nUPDATE app.u2 SET c = 1;\n";
        let a: Answers = vec![(exists(Object::Column("app.t".into(), "b".into())), 0), (exists(Object::Column("app.u".into(), "c".into())), 1)]
            .into_iter().collect();
        let (c, _) = taken(sql, &a);
        assert_eq!(c["app.t_old"].ddl, vec!["alter: add b", "alter: drop x"]);
        // x went with t to t_old: the new t has none to drop
        assert_eq!(c["app.t"].ddl, vec!["alter: add x", "alter: rename to t_old", "create (id, b)"]);
        let u2 = &c["app.u2"];
        assert_eq!(u2.ddl, vec!["drop", "create (id)", "alter: add c"]);
        assert_eq!((u2.inserts, &u2.uncounted), (2, &un(&[Dml::Update])));
    }

    /// [plan] TRUNCATE with the DML: the rows each table has, counted; a table the database
    /// has not, its statements
    #[test]
    fn a_truncate_is_its_tables_rows() {
        let sql = "TRUNCATE app.t, ONLY app.u;\nTRUNCATE app.n;\nDELETE FROM app.t WHERE id = 1;\n";
        let a: Answers = vec![(rel("app.t"), 1), (rel("app.u"), 1), (rel("app.n"), 0), (count("app.t", "SELECT count(*) FROM app.t"), 7),
            (count("app.u", "SELECT count(*) FROM ONLY app.u"), 2), (count("app.t", "SELECT count(*) FROM app.t WHERE id = 1"), 1)]
            .into_iter().collect();
        let (c, _) = taken(sql, &a);
        assert_eq!(describe(&c["app.t"]), "dml delete 1, truncate 7");
        assert_eq!(describe(&c["app.u"]), "dml truncate 2");
        assert_eq!(describe(&c["app.n"]), "dml truncate 1 statement");
    }
}
