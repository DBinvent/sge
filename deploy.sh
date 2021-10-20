#!/bin/bash

#dest=/opt/www/rumba/dist
#rdbm="$(System.ArtifactsDirectory)/_vlad4kr7.rdbm/drop/$(Build.BuildId).zip"
#echo "Deploy RDBM: $1 to $dest "
#pwd && ls -lah
#mkdir -p $dest
#cp $rdbm $dest/rdbm-latest.zip
#cd $dest
# rm -f *.rpm *.deb
#unzip -o rdbm-latest.zip || exit 1
#bash -e ./deploy.sh

latest=rdbm-unix-latest

function md5line() {
  cm=$(md5sum $latest.$2)
  m=${cm:0:32}
  echo "rdbm|unix|$1|$2|$latest.$2|$m" >> rdbm.version
}

# run repo.sh on prod before deploy.sh

echo "Deploy RDBM: prepare archives"
pwd && ls -lah

list="rdbm evaluation.license LICENSE lib-licenses.txt"

rm -f $latest.*

# VERSION
cat rdbm.version
vv=$(cat rdbm.version)
rm rdbm.version

# ZIP
zip $latest.zip $list && echo "checking $latest.zip ..." && unzip -l $latest.zip
md5line $vv zip

# TAR
tar -cvzf $latest.tar.gz $list && echo "checking $latest.tar.gz ..." && tar -ztvf $latest.tar.gz
md5line $vv tar.gz

echo "checking content ..."  && cat rdbm.version

rm -rf *.sh rdbm lib-licenses.txt
# *.deb *.rpm
