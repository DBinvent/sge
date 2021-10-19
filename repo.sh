#!/bin/bash

# run repo.sh on prod before deploy.sh

vv=$(cat rdbm.version)

echo "Deploy RDBM v$vv: to repositories at $1" #https://www.maxoberberger.net/blog/2013/05/creating-your-own-apt-repository.html
deb=rdbm-$vv.deb
dpkg-sig --sign builder $deb

reprepro -Vb $1 includedeb rumba $(dirname $deb)/$deb



