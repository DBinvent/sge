#!/bin/bash
echo "Deploy RDBM: prepare archives"
pwd && ls -lah

list="rdbm evaluation.license LICENSE lib-licenses.txt"

rm -f rdbm-latest.*

zip rdbm-latest.zip $list
unzip -l rdbm-latest.zip

tar -cvzf rdbm-latest.tar.gz $list
echo "checking rdbm-latest.tar.gz ..."
tar -ztvf rdbm-latest.tar.gz

rm $list
