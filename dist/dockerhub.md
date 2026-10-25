# Schema Guard (sge): PostgreSQL schema migration

A static `sge` and the `psql` it applies SQL files with, on Alpine - about 12 MB.
Open source, MIT: [DBinvent/sge](https://github.com/DBinvent/sge).

- A Flyway replacement with no JVM: versioned scripts (`V1_2__init.sql`) and
  repeatables (`R__view.sql`) run as they are
- Declarative and imperative, mixed: a whole schema in one YAML file, turned into
  SQL for you, beside plain SQL scripts
- History in the target database: every environment knows which versions it
  applied, with which checksum (`sge_history`)

Docs: [dbinvent.com/sge](https://www.dbinvent.com/sge/) - [downloads](https://www.dbinvent.com/sge/downloads)
(apt, static binary, crate).

## Tags

`2.1.0`, ... (a version), `2.0` (its latest), `latest`. x86-64.

## Use

The image runs `sge` in `/work`: mount the migrations there.

```sh
docker run --rm -v "$PWD:/work" dbinvent/sge \
    --db_url=postgres://app:secret@db.example.com/app --source_dir=. migrate

# a config file in the same directory, a database on the same machine
docker run --rm --network host -v "$PWD:/work" dbinvent/sge -c sge.cfg migrate

docker run --rm dbinvent/sge help migrate
```

`sge.cfg` takes the options by the same names, without the `--`:

```
db_host=localhost
db_name=app
db_user=app
source_dir=.
```

The help and the messages are in English, Russian, and - not reviewed - French,
German, Simplified Chinese and Japanese, by `LANG`. A container has none of the
host's environment: pass it on with `-e LANG`, or name one, `-e LANG=de_DE.UTF-8`.

`git` is not in the image: a git script source (`source_git`) needs an image built
`FROM dbinvent/sge` that adds it (`apk add git openssh-client`).

On a [YT Cluster](https://hub.docker.com/r/dbinvent/yt), a migration applies to
the node it runs on: run it against each node in turn.

## License

MIT. The image carries it in `/usr/share/doc/schema-guard/LICENSE`. PostgreSQL's
client and Alpine are under their own licenses.
