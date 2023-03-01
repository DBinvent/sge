--test
-- test --single-transaction=true
-- --test
-- -- test --single-transaction=false
select * from rdbm_history where source_type = 'D';