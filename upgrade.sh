#!/bin/bash

dist=/opt/www/rumba/

# update zip
zip -m evaluation.license $dist/rdbm-latest.zip

# update tar
mkdir -p latest
cd latest
unzip $dist/rdbm-latest.zip
rm $dist/rdbm-latest.tar.gz
tar -czf $dist/rdbm-latest.tar.gz *
