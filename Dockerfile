# dbinvent/sge - Schema Guard: a static (musl) sge, and the psql it applies SQL
# files with, on Alpine - the smallest base that has psql. Built from this
# directory: docker build -t dbinvent/sge .
#
#   docker run --rm -v "$PWD:/work" dbinvent/sge --db_url=postgres://app@db/app --source_dir=. migrate
#
# git is not in it: a git script source (source_git) needs an image that adds it,
# FROM dbinvent/sge with apk add git openssh-client.
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
RUN apk add --no-cache postgresql18-client
COPY --from=build /sge /usr/local/bin/sge
COPY LICENSE /usr/share/doc/schema-guard/LICENSE
WORKDIR /work
ENTRYPOINT ["sge"]
CMD ["help"]
