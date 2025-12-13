job "globibot" {
  group "globibot" {
    network {
      mode = "bridge"
      hostname = "globibot"

      port "subscriber" {}
      port "rpc" {}
    }

    volume "globibot-data" {
      type      = "host"
      source    = "globibot-data"
      read_only = true
    }

    task "bot" {
      driver = "docker"

      config {
        image = "globidocker/globibot"
        command = "/globibot"
      }

      env {
        SUBSCRIBER_ADDR = "0.0.0.0:${NOMAD_PORT_subscriber}"
        RPC_ADDR        = "0.0.0.0:${NOMAD_PORT_rpc}"
        RUST_LOG        = "globibot_bot=debug"
      }

       template {
        data = <<EOH
{{ with nomadVar "nomad/jobs/globibot/globibot/bot" }}
  {{range .}}
    {{.Key}}={{.Value}}
  {{end}}
{{ end }}
EOH
        destination = "${NOMAD_SECRETS_DIR}/env.vars"
        change_mode = "restart"
        env         = true
      }
    }

    task "plugin-rateme" {
      driver = "docker"

      config {
        image = "globidocker/globibot"
        command = "/plugins/rateme"
      }

      volume_mount {
        volume      = "globibot-data"
        destination = "/globibot-data"
      }

      env {
        SUBSCRIBER_ADDR = "globibot:${NOMAD_PORT_subscriber}"
        RPC_ADDR        = "globibot:${NOMAD_PORT_rpc}"
        RUST_LOG        = "globibot_plugin_rateme=debug"
        RATEME_IMG_PATH = "/globibot-data/plugin-rateme-imgs"
      }

      template {
        data = <<EOH
{{ with nomadVar "nomad/jobs/globibot/globibot/plugin-rateme" }}
  {{range .}}
    {{.Key}}={{.Value}}
  {{end}}
{{ end }}
EOH
        destination = "${NOMAD_SECRETS_DIR}/env.vars"
        change_mode = "restart"
        env         = true
      }
    }

    task "plugin-tuck" {
      driver = "docker"

      config {
        image = "globidocker/globibot"
        command = "/plugins/tuck"
      }

      volume_mount {
        volume      = "globibot-data"
        destination = "/globibot-data"
      }

      env {
        SUBSCRIBER_ADDR = "globibot:${NOMAD_PORT_subscriber}"
        RPC_ADDR        = "globibot:${NOMAD_PORT_rpc}"
        RUST_LOG        = "globibot_plugin_tuck=debug"
        TUCK_IMG_PATH   = "/globibot-data/plugin-tuck-imgs"
      }

      template {
        data = <<EOH
{{ with nomadVar "nomad/jobs/globibot/globibot/plugin-tuck" }}
  {{range .}}
    {{.Key}}={{.Value}}
  {{end}}
{{ end }}
EOH
        destination = "${NOMAD_SECRETS_DIR}/env.vars"
        change_mode = "restart"
        env         = true
      }
    }

    task "plugin-slap" {
      driver = "docker"

      config {
        image = "globidocker/globibot"
        command = "/plugins/slap"
      }

      volume_mount {
        volume      = "globibot-data"
        destination = "/globibot-data"
      }

      env {
        SUBSCRIBER_ADDR = "globibot:${NOMAD_PORT_subscriber}"
        RPC_ADDR        = "globibot:${NOMAD_PORT_rpc}"
        RUST_LOG        = "globibot_plugin_slap=debug"
        SLAP_IMG_PATH   = "/globibot-data/plugin-slap-imgs"
      }

      template {
        data = <<EOH
{{ with nomadVar "nomad/jobs/globibot/globibot/plugin-slap" }}
  {{range .}}
    {{.Key}}={{.Value}}
  {{end}}
{{ end }}
EOH
        destination = "${NOMAD_SECRETS_DIR}/env.vars"
        change_mode = "restart"
        env         = true
      }
    }

    task "plugin-movienight" {
      driver = "docker"

      config {
        image = "globidocker/globibot"
        command = "/plugins/movienight"
      }

      volume_mount {
        volume      = "globibot-data"
        destination = "/globibot-data"
      }

      env {
        SUBSCRIBER_ADDR = "globibot:${NOMAD_PORT_subscriber}"
        RPC_ADDR        = "globibot:${NOMAD_PORT_rpc}"
        RUST_LOG        = "globibot_plugin_movienight=debug"
        ART_IMG_PATH    = "/globibot-data/plugin-movienight-imgs"
      }

      template {
        data = <<EOH
{{ with nomadVar "nomad/jobs/globibot/globibot/plugin-movienight" }}
  {{range .}}
    {{.Key}}={{.Value}}
  {{end}}
{{ end }}
EOH
        destination = "${NOMAD_SECRETS_DIR}/env.vars"
        change_mode = "restart"
        env         = true
      }
    }

    task "plugin-llm" {
      driver = "docker"

      config {
        image = "globidocker/globibot"
        command = "/plugins/llm"
      }

      volume_mount {
        volume      = "globibot-data"
        destination = "/globibot-data"
      }

      env {
        SUBSCRIBER_ADDR = "globibot:${NOMAD_PORT_subscriber}"
        RPC_ADDR        = "globibot:${NOMAD_PORT_rpc}"
        RUST_LOG        = "globibot_plugin_llm=debug"
      }

      template {
        data = <<EOH
{{ with nomadVar "nomad/jobs/globibot/globibot/plugin-llm" }}
  {{range .}}
    {{.Key}}={{.Value}}
  {{end}}
{{ end }}
EOH
        destination = "${NOMAD_SECRETS_DIR}/env.vars"
        change_mode = "restart"
        env         = true
      }
    }
  }
}
