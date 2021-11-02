#!/bin/bash

# runs repo.sh on prod before deploy.sh at /opt/www/rumba/dist
# ATTENTION! ключи генерировать рутом! подписание идет рутом, иначе ключи не видны при добавленнии в репо
# sudo gpg --gen-key && sudo gpg --armor --export support@dbinvent.com > /opt/www/DBinventLLC.gpg.key
# https://github.com/emk/rust-musl-builder

vv=$(cat rdbm.version)

echo "Deploy RDBM v$vv: to repositories at $1" #https://www.maxoberberger.net/blog/2013/05/creating-your-own-apt-repository.html
deb=rdbm_${vv}_amd64.deb
sudo dpkg-sig --sign builder $deb
sudo dpkg-sig -c $deb

grep -v "#" distro.lst | while read line
do
 if [ ${#line} -gt 1 ]; then
  echo "APT to [$line] distro"
  sudo mkdir -p $1/$line/conf
  sudo cp -f distributions $1/$line/conf
  sudo reprepro -Vb $1/$line -S contrib -C "non-free" includedeb $line $(dirname $deb)/$deb
 fi
done

rm $deb

gpg_pub_key="DBinventLLC.gpg.key"
echo "Upgrade pub key: $gpg_pub_key" && rm -f $gpg_pub_key && ln -s /opt/www/$gpg_pub_key $gpg_pub_key

rpmp=rdbm-${vv}-1.x86_64.rpm
echo "Signing $rpmp"
sudo rpm --addsign $rpmp 2>&1
rpm -qiRp $rpmp 2>&1
rpm -Kv  $rpmp 2>&1 || true

rpml=rdbm-latest.x86_64.rpm
rm -f $rpml
ln -s $rpmp $rpml

#sudo yum install -y https://yum.dbinvent.com/rdbm-latest.x86_64.rpm
sudo cp -f $rpmp /opt/www/yum/$rpml
sudo createrepo --update -n $rpml /opt/www/yum
