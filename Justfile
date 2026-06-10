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
    MARSALA__PROXY__ENABLED=true MARSALA__MITM__ENABLED=true cargo run -p marsala -- serve

logs:
    cargo run -p marsala -- logs tail --lines 80 --follow
