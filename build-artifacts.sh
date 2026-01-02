set -e

docker run \
    --rm \
    -it \
    -v '.:/usr/src/globibot/' \
    -v 'globibot-target:/usr/src/globibot/target' \
    -v 'globibot-registry:/usr/local/cargo/registry' \
    -e RUSTFLAGS='-C target-feature=-crt-static' \
    globibot-builder sh ./build.sh
