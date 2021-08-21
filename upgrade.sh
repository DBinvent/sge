#!/bin/bash

dist=/opt/www/rumba/dist

unzip -l $dist/rdbm-latest.zip
echo "Upgrade: $dist with `wc -l evaluation.license`"
zip -m evaluation.license $dist/rdbm-latest.zip
unzip -t $dist/rdbm-latest.zip

echo "Upgrading TAR..."
rm -rf latest
mkdir latest
cd latest
unzip $dist/rdbm-latest.zip
rm $dist/rdbm-latest.tar.gz
tar -cvzf $dist/rdbm-latest.tar.gz *

echo "checking $dist/rdbm-latest.tar.gz ..."
tar -ztvf $dist/rdbm-latest.tar.gz
