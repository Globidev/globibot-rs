FROM alpine:3.13

RUN apk add openssl libgcc

COPY ./target-builder/release/globibot-bot /globibot
COPY ./target-builder/release/globibot-plugin-openai /plugins/openai
COPY ./target-builder/release/globibot-plugin-rateme /plugins/rateme
COPY ./target-builder/release/globibot-plugin-ping /plugins/ping
COPY ./target-builder/release/globibot-plugin-tuck /plugins/tuck

CMD "/globibot"
