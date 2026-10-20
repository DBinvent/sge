#!/bin/bash

dist=/opt/www/sge/dist
latest=sge-unix-latest

unzip -t $dist/$latest.zip

echo "Upgrading TAR..."
rm -rf latest
mkdir latest
cd latest
unzip $dist/$latest.zip
rm $dist/$latest.tar.gz
tar -cvzf $dist/$latest.tar.gz *

echo "Checking upgraded $dist/$latest.tar.gz ..."
tar -ztvf $dist/$latest.tar.gz

cd $dist
echo "Upgrading sge.version..." && cat sge.version
cat sge.version | while read line
do
 IFS='|' read -r -a va <<< "$line"
 file="${va[4]}"
 if [ -f "$file" ]; then
  newmd5=$(md5sum $file)
  echo "upgrade MD5 for $file from ${va[5]} to ${newmd5:0:32} in sge.version"
  sed -i "s/${va[5]}/${newmd5:0:32}/" sge.version
 fi
done
