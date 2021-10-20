#!/bin/bash

# run repo.sh on prod before deploy.sh

vv=$(cat rdbm.version)

echo "Deploy RDBM v$vv: to repositories at $1" #https://www.maxoberberger.net/blog/2013/05/creating-your-own-apt-repository.html
deb=rdbm_${vv}_amd64.deb
dpkg-sig --sign builder $deb
dpkg-sig -c $deb

echo DEBUG

echo reprepro -Vb $1 includedeb rumba $(dirname $deb)/$deb

echo rm $deb

gpg_pub_key="DBinventLLC.gpg.key"
rm -f $gpg_pub_key
ln -s /opt/www/$gpg_pub_key $gpg_pub_key

