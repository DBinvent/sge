#!/bin/bash
dist=/opt/www/rumba/
echo "Upgrade RDBM artifacts at: $dist"

set -x

# update zip
zip -m evaluation.license $dist/rdbm-latest.zip

echo "Upgraded ZIP RDBM artifact. Upgrading TAR..."
rm -rf latest
mkdir latest
cd latest
unzip $dist/rdbm-latest.zip
rm $dist/rdbm-latest.tar.gz
tar -czf $dist/rdbm-latest.tar.gz *
