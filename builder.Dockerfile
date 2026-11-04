FROM rustlang/rust:nightly-alpine

RUN apk add build-base openssl-dev mold

WORKDIR /usr/src/globibot
