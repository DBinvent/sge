# dbinvent/sge - Schema Guard: a static (musl) sge, and the psql it applies SQL
# files with, on Alpine - the smallest base that has psql. Built from this
# directory: docker build -t dbinvent/sge .
#
#   docker run --rm -v "$PWD:/work" dbinvent/sge --db_url=postgres://app@db/app --source_dir=. migrate
#
# It runs as the user sge, uid/gid 1000 - on most hosts the one who owns the mounted
# folder, so what sge writes there (dry_run_to) is theirs. Another uid: --user "$(id -u):$(id -g)".
#
# git is not in it: a git script source (source_git) needs an image that adds it,
# FROM dbinvent/sge with USER root, apk add git openssh-client, USER sge.
FROM clux/muslrust:stable AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked --target x86_64-unknown-linux-musl --bin sge \
    && cp target/x86_64-unknown-linux-musl/release/sge /sge \
    && /sge --version

FROM alpine:3.23
ARG SGE_VERSION=unknown
LABEL org.opencontainers.image.title="Schema Guard" \
      org.opencontainers.image.description="PostgreSQL schema migration: versioned SQL scripts and declarative YAML, tracked per environment" \
      org.opencontainers.image.url="https://www.dbinvent.com/sge" \
      org.opencontainers.image.source="https://github.com/DBinvent/sge" \
      org.opencontainers.image.vendor="DBinvent" \
      org.opencontainers.image.version="${SGE_VERSION}" \
      org.opencontainers.image.licenses="MIT"
# busybox from edge: 3.23's 1.37 has wget's CVE-2025-60876, fixed in 1.38, which no
# stable Alpine has yet. It needs only musl, so nothing else comes from edge. Back to
# 3.23's busybox (drop the upgrade) once a stable release has the fix.
RUN apk add --no-cache postgresql18-client \
    && apk upgrade --no-cache -X https://dl-cdn.alpinelinux.org/alpine/edge/main \
        busybox busybox-binsh ssl_client \
    && apk info -e 'busybox>=1.38' \
    && addgroup -g 1000 sge && adduser -D -u 1000 -G sge -h /home/sge sge
COPY --from=build /sge /usr/local/bin/sge
COPY LICENSE /usr/share/doc/schema-guard/LICENSE
WORKDIR /work
USER sge
# a command run once, not a service: nothing to health-check
HEALTHCHECK NONE
ENTRYPOINT ["sge"]
CMD ["help"]
