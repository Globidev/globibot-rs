FROM alpine:3.23

RUN apk add openssl libssl3 libgcc

COPY ./x64-artifacts/globibot-bot /globibot
COPY ./x64-artifacts/globibot-plugin-openai /plugins/openai
COPY ./x64-artifacts/globibot-plugin-rateme /plugins/rateme
COPY ./x64-artifacts/globibot-plugin-ping /plugins/ping
COPY ./x64-artifacts/globibot-plugin-tuck /plugins/tuck
COPY ./x64-artifacts/globibot-plugin-lang-detect /plugins/lang-detect
COPY ./x64-artifacts/globibot-plugin-slap /plugins/slap
COPY ./x64-artifacts/globibot-plugin-movienight /plugins/movienight
COPY ./x64-artifacts/globibot-plugin-llm /plugins/llm

CMD ["/globibot"]
