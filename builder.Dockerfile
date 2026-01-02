FROM rustlang/rust:nightly-alpine

RUN apk add build-base openssl-dev

WORKDIR /usr/src/globibot
