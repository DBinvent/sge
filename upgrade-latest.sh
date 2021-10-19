#!/bin/bash

dist=/opt/www/rumba/dist

latest=rdbm-unix-latest

unzip -l $dist/$latest.zip
echo "Upgrade: $dist with `wc -l evaluation.license`"
zip -m $dist/$latest.zip evaluation.license
unzip -t $dist/$latest.zip

mv -f evaluation.license $dist/
rm -rf latest

echo "Upgrading TAR..."
mkdir latest
cd latest
unzip $dist/$latest.zip
rm $dist/$latest.tar.gz
tar -cvzf $dist/$latest.tar.gz *

echo "checking $dist/$latest.tar.gz ..."
tar -ztvf $dist/$latest.tar.gz


cat rdbm.version | while read line
do
 IFS='|' read -r -a va <<< "$line"
 file="${va[4]}"
 if [ -f "$file" ]; then
  newmd5=$(md5sum $file)
  echo "upgrade MD5 for $file from ${va[5]} to ${newmd5:0:32} in rdbm.version"
  sed -i "s/${va[5]}/${newmd5:0:32}/" rdbm.version
 fi
done

