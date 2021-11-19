--test
-- test --single-transaction=false
\echo test_schema.test_table
insert into test_schema.test_table (test) values ('extra');
select * from test_schema.test_table;

\echo test_schema.test_ref_table
insert into test_schema.test_ref_table (test) values ('extra');
select * from test_schema.test_ref_table;