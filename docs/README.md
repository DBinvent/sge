Examples: one table, changed the two ways sge takes a schema change.

- `S1_0__schema.yaml` - the schema declared in YAML; sge works out the SQL.
- `S1_0__schema_alise.yaml`, `S1_0__schema_john.yaml` - two developers' changes
  to it, a column each. YAML merges like any text, so a team edits one schema
  together.
- `V1_0__schema_alise.sql`, `V1_0__schema_john.sql` - the same two changes as
  versioned, hand-written SQL scripts: run once each and recorded in the
  history - the reviewed, signed-off way.

A file's name says what it is: its kind (`S` schema, `V` version, `R`
re-runnable, `P` pre-deploy, `D` data), its version, then `__` and any name.
