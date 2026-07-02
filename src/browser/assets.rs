pub(super) const BROWSER_DOCKERFILE: &str = r#"FROM kechangdev/browser-vnc:latest

# bash/procps keep upstream startup scripts compatible; socat exposes a stable CDP port.
# xwd/imagemagick provide deterministic X11 framebuffer screenshots for noVNC/CDP proof.
RUN apk add --no-cache bash procps socat python3 net-tools curl xwd imagemagick

RUN python3 -c "from pathlib import Path; root=Path('/opt/noVNC/utils/websockify'); [p.write_text(p.read_text().replace('.fromstring(', '.frombytes(').replace('.tostring(', '.tobytes(')) for p in root.rglob('*.py')]"

COPY docker-git-browser-start.sh /usr/local/bin/docker-git-browser-start.sh
RUN chmod +x /usr/local/bin/docker-git-browser-start.sh

ENTRYPOINT ["/usr/local/bin/docker-git-browser-start.sh"]
"#;

pub(super) const BROWSER_START_SCRIPT: &str = r#"#!/usr/bin/env bash
set -euo pipefail

rm -f /data/SingletonLock /data/SingletonCookie /data/SingletonSocket || true

for supervisor_file in /etc/supervisor.d/*.ini /etc/supervisor/conf.d/*.conf; do
  if [[ -f "$supervisor_file" ]]; then
    sed -i \
      -e 's|-forever -usepw -display :99 -rfbport 5900|-forever -nopw -shared -display :99 -rfbport 5900|g' \
      -e 's|x11vnc -forever -usepw|x11vnc -forever -nopw -shared|g' \
      "$supervisor_file"
  fi
done

socat TCP-LISTEN:9223,fork,reuseaddr TCP:127.0.0.1:9222 &

exec /start.sh
"#;

pub(super) const NOVNC_PROXY_DOCKERFILE: &str = r#"FROM kechangdev/browser-vnc:latest

RUN apk add --no-cache bash curl python3

RUN python3 -c "from pathlib import Path; root=Path('/opt/noVNC/utils/websockify'); [p.write_text(p.read_text().replace('.fromstring(', '.frombytes(').replace('.tostring(', '.tobytes(')) for p in root.rglob('*.py')]"

COPY docker-git-novnc-proxy-start.sh /usr/local/bin/docker-git-novnc-proxy-start.sh
RUN chmod +x /usr/local/bin/docker-git-novnc-proxy-start.sh

ENTRYPOINT ["/usr/local/bin/docker-git-novnc-proxy-start.sh"]
"#;

pub(super) const NOVNC_PROXY_START_SCRIPT: &str = r#"#!/usr/bin/env bash
set -euo pipefail

target="${BROWSER_CONNECTION_VNC_ENDPOINT:?BROWSER_CONNECTION_VNC_ENDPOINT is required}"
listen_port="${BROWSER_CONNECTION_NOVNC_PORT:-6080}"
web_root="${BROWSER_CONNECTION_NOVNC_WEB:-/usr/share/novnc}"

if [[ ! -d "$web_root" && -d /usr/share/noVNC ]]; then
  web_root=/usr/share/noVNC
fi
if [[ ! -d "$web_root" && -d /usr/share/webapps/novnc ]]; then
  web_root=/usr/share/webapps/novnc
fi

if [[ -x /opt/noVNC/utils/novnc_proxy ]]; then
  exec /opt/noVNC/utils/novnc_proxy \
    --listen "$listen_port" \
    --vnc "$target"
fi

if command -v websockify >/dev/null 2>&1; then
  exec websockify --web "$web_root" "0.0.0.0:${listen_port}" "$target"
fi

exec python3 -m websockify --web "$web_root" "0.0.0.0:${listen_port}" "$target"
"#;
