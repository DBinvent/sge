#!/bin/bash

#dest=/opt/www/sge/dist
#sge="$(System.ArtifactsDirectory)/_DBinvent.sge/drop/$(Build.BuildId).zip"
#echo "Deploy SGE: $1 to $dest "
#pwd && ls -lah
#mkdir -p $dest
#cp $sge $dest/sge-latest.zip
#cd $dest
# rm -f *.rpm *.deb
#unzip -o sge-latest.zip || exit 1
#bash -e ./deploy.sh

latest=sge-unix-latest

function md5line() {
  cm=$(md5sum $latest.$2)
  m=${cm:0:32}
  echo "sge|unix|$1|$2|$latest.$2|$m" >> sge.version
}

# run repo.sh on prod before deploy.sh

echo "Deploy SGE: prepare archives"
pwd && ls -lah

list="sge LICENSE lib-licenses.txt"

rm -f $latest.*

# VERSION
cat sge.version
vv=$(cat sge.version)
rm sge.version

# ZIP
zip $latest.zip $list && echo "checking $latest.zip ..." && unzip -l $latest.zip
md5line $vv zip

# TAR
tar -cvzf $latest.tar.gz $list && echo "checking $latest.tar.gz ..." && tar -ztvf $latest.tar.gz
md5line $vv tar.gz

echo "checking content ..."  && cat sge.version

rm -rf *.sh sge lib-licenses.txt
# *.deb *.rpm
