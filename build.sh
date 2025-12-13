set -e

cargo build --release

cp target/release/globibot-bot artifacts/globibot
mkdir -p artifacts/plugins

for plugin in target/release/globibot-plugin-*[!.d]; do
    base_name=$(basename "$plugin")
    plugin_name=${base_name#globibot-plugin-}
    cp "$plugin" "artifacts/plugins/$plugin_name"
done
