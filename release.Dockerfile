FROM alpine:3.23

RUN apk add openssl libssl3 libgcc

COPY ./arm64-artifacts/ /

CMD ["/globibot"]
