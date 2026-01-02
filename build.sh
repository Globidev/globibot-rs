set -e

cargo build --release

mkdir -p arm64-artifacts/plugins

cp target/release/globibot-bot arm64-artifacts/globibot

for plugin in target/release/globibot-plugin-*[!.d]; do
    base_name=$(basename "$plugin")
    plugin_name=${base_name#globibot-plugin-}
    cp "$plugin" "arm64-artifacts/plugins/$plugin_name"
done
