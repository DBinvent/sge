--test
-- test --single-transaction=false
\echo test_schema.test_table
select * from test_schema.test_table;

\echo test_schema.test_ref_table
select * from test_schema.test_ref_table;