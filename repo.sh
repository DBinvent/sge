#!/bin/bash

# runs repo.sh on prod before deploy.sh at /opt/www/rumba/dist
# ATTENTION! ключи генерировать рутом! подписание идет рутом, иначе ключи не видны при добавленнии в репо
# sudo gpg --gen-key && sudo gpg --armor --export support@dbinvent.com > /opt/www/DBinventLLC.gpg.key

vv=$(cat rdbm.version)

echo "Deploy RDBM v$vv: to repositories at $1" #https://www.maxoberberger.net/blog/2013/05/creating-your-own-apt-repository.html
deb=rdbm_${vv}_amd64.deb
sudo dpkg-sig --sign builder $deb
sudo dpkg-sig -c $deb

sudo mv -f distributions $1/conf
sudo reprepro -Vb $1 -S contrib -C "non-free" includedeb focal $(dirname $deb)/$deb

rm $deb

gpg_pub_key="DBinventLLC.gpg.key"
echo "Upgrade pub key: $gpg_pub_key" && rm -f $gpg_pub_key && ln -s /opt/www/$gpg_pub_key $gpg_pub_key

rpmp=rdbm-${vv}-1.x86_64.rpm
echo "Signing $rpmp"
sudo rpm --addsign $rpmp
rpm -qiRp $rpmp
rpm -Kv  $rpmp

rpml=rdbm-latest.x86_64.rpm
rm -f $rpml
ln -s $rpmp $rpml
ll
