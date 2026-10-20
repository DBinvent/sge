#!/bin/bash
set -Eeu

SCRIPT_DIR=$(dirname $(realpath "$0"))
cd $SCRIPT_DIR

echo "Checking the version:> sge -V"
../../sge -V
echo
echo "Creating history table for future migrations:> sge -c test.cfg prepare; with params:"
cat test.cfg
echo
../../sge -c test.cfg --db_port $1 prepare

echo
echo "Dumping the schema in YAML format:> sge -c test.cfg snapshot"
../../sge -c test.cfg --db_port $1 --snapshot_to=test_result.out.yaml snapshot

echo "Comparing YAML files: diff output vs expected"
diff -B test_result.yaml test_result.out.yaml

echo
echo "Dumping the schema in SQL format:>  pg_dump -s -h "
pg_dump -s -h localhost -U postgres -p $1 | grep -Ev "\-\-|SET|COMMENT ON|pg_catalog.set_config" > test_result.out.sql

echo "Comparing SQL files: diff output vs expected"
diff -B test_result.sql test_result.out.sql

echo