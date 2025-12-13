FROM rustlang/rust:nightly-alpine

RUN apk add build-base openssl-dev

WORKDIR /usr/src/globibot

VOLUME /usr/src/globibot/artifacts

