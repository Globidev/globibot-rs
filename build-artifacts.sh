set -e

docker run \
    --rm \
    -it \
    --platform linux/amd64 \
    -v '.:/usr/src/globibot/' \
    -v 'globibot-target:/usr/src/globibot/target' \
    -v 'globibot-registry:/usr/local/cargo/registry' \
    -v './x64-artifacts:/usr/src/globibot/artifacts' \
    -e RUSTFLAGS='-C target-feature=-crt-static' \
    globibot-builder sh ./build.sh
