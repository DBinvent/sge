# Schema Guard Enterprise (sge): PostgreSQL schema migration

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

`2.3.0`, ... (a version), `2.3` (its latest), `latest`. x86-64.

## Use

The image runs `sge` in `/work`: mount the migrations there.

It runs as the user `sge`, uid/gid 1000: a file sge writes there (`dry_run_to`)
needs `/work` writable by that uid - another uid: `--user "$(id -u):$(id -g)"`.

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

## GitHub Actions

A sample workflow - plan, approval, migrate - with this image alone: see
[the README](https://github.com/DBinvent/sge#github-actions).

## SBOM

The image has SBOM and provenance attestations - Alpine's packages and sge's crates:
`docker buildx imagetools inspect dbinvent/sge --format '{{json .SBOM}}'`. sge's own
CycloneDX SBOM is in the image too, `/usr/share/doc/schema-guard/sbom.cdx.json`.

## License

MIT. The image carries it in `/usr/share/doc/schema-guard/LICENSE`. PostgreSQL's
client and Alpine are under their own licenses.
