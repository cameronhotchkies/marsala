set dotenv-load := false

default:
    just --list

check:
    cargo test --locked

test:
    cargo test --locked

serve:
    cargo run -p marsala -- serve

mitm-ca-init:
    cargo run -p marsala -- mitm ca init

mitm-serve:
    MARSALA__PROXY__ENABLED=true MARSALA__MITM__ENABLED=true MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com cargo run -p marsala -- serve

mitm-serve-capture preview_bytes='65536':
    MARSALA__PROXY__ENABLED=true MARSALA__MITM__ENABLED=true MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com MARSALA__LOGGING__CAPTURE_MITM_PAYLOADS=true MARSALA__LOGGING__CAPTURE_MITM_WEBSOCKET_FRAMES=true MARSALA__LOGGING__MITM_PAYLOAD_PREVIEW_BYTES={{preview_bytes}} MARSALA__LOGGING__MITM_WEBSOCKET_FRAME_PREVIEW_BYTES={{preview_bytes}} cargo run -p marsala -- serve

ui preview_bytes='65536':
    @echo "Marsala interception UI: http://127.0.0.1:8787/ui"
    MARSALA__PROXY__ENABLED=true MARSALA__MITM__ENABLED=true MARSALA__MITM__ALLOW_HOSTS=chatgpt.com,ab.chatgpt.com MARSALA__LOGGING__CAPTURE_MITM_PAYLOADS=true MARSALA__LOGGING__CAPTURE_MITM_WEBSOCKET_FRAMES=true MARSALA__LOGGING__MITM_PAYLOAD_PREVIEW_BYTES={{preview_bytes}} MARSALA__LOGGING__MITM_WEBSOCKET_FRAME_PREVIEW_BYTES={{preview_bytes}} cargo run -p marsala -- serve

mitm-codex prompt='Reply with one short sentence.':
    env HTTPS_PROXY=http://127.0.0.1:8788 https_proxy=http://127.0.0.1:8788 HTTP_PROXY= http_proxy= ALL_PROXY= all_proxy= NO_PROXY= no_proxy= CODEX_CA_CERTIFICATE="$PWD/certs/marsala-ca.pem" SSL_CERT_FILE="$PWD/certs/marsala-ca.pem" codex exec --skip-git-repo-check -c 'approval_policy="never"' "{{prompt}}"

codex-env:
    @printf '%s\n' \
        'export HTTPS_PROXY=http://127.0.0.1:8788' \
        'export https_proxy=http://127.0.0.1:8788' \
        'export HTTP_PROXY=' \
        'export http_proxy=' \
        'export ALL_PROXY=' \
        'export all_proxy=' \
        'export NO_PROXY=' \
        'export no_proxy=' \
        'export CODEX_CA_CERTIFICATE={{justfile_directory()}}/certs/marsala-ca.pem' \
        'export SSL_CERT_FILE={{justfile_directory()}}/certs/marsala-ca.pem'

logs:
    cargo run -p marsala -- logs tail --lines 80 --follow

payload-logs:
    rg 'mitm_payload|mitm_websocket_frame' logs/events.jsonl
