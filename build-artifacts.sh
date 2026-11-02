set -e

# -t only with a terminal: without one (scripts, agents) docker refuses to start
TTY=
if [ -t 0 ]; then TTY=-t; fi

docker run \
    --rm \
    -i $TTY \
    -v '.:/usr/src/globibot/' \
    -v 'globibot-target:/usr/src/globibot/target' \
    -v 'globibot-registry:/usr/local/cargo/registry' \
    -e RUSTFLAGS='-C target-feature=-crt-static' \
    globibot-builder sh ./build.sh
