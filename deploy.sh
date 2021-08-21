#!/bin/bash
echo "Deploy RDBM: prepare archives"
set -x

pwd
ls -lah

zip rdbm-latest.zip rdbm evaluation.license LICENSE lib-licenses.txt
tar -czf rdbm-latest.tar.gz rdbm evaluation.license LICENSE lib-licenses.txt

rm rdbm evaluation.license LICENSE lib-licenses.txt
