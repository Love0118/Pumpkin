#!/usr/bin/env bash

set -Eeuo pipefail

REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
SERVER_BINARY="$REPO_ROOT/target/release/pumpkin"
RUN_DIR="${PUMPKIN_ENTITY_PACKET_RUN_DIR:-$REPO_ROOT/.entity-packet-server}"
PLAYER_NAME="${PUMPKIN_ENTITY_PACKET_PLAYER:-R_Rust}"
PLAYER_UUID="${PUMPKIN_ENTITY_PACKET_UUID:-}"
SERVER_LOG="$RUN_DIR/pumpkin.log"
PID_FILE="$RUN_DIR/pumpkin.pid"

if ! command -v jq >/dev/null 2>&1; then
    printf '%s\n' 'jq is required to configure ops.json.' >&2
    exit 1
fi

server_pids() {
    local pid args exe
    while read -r pid args; do
        [[ "$pid" != "$$" ]] || continue
        [[ -r "/proc/$pid/exe" ]] || continue
        exe="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
        if [[ "$exe" == "$REPO_ROOT/target/release/pumpkin" \
            || "$exe" == "$REPO_ROOT/target/debug/pumpkin" \
            || "$args" == *"$REPO_ROOT/target/release/pumpkin"* \
            || "$args" == *"$REPO_ROOT/target/debug/pumpkin"* ]]; then
            printf '%s\n' "$pid"
        fi
    done < <(ps -eo pid=,args=)
}

stop_existing_servers() {
    local pids pid
    mapfile -t pids < <(server_pids)
    ((${#pids[@]} == 0)) && return

    printf 'Stopping Pumpkin server processes: %s\n' "${pids[*]}"
    kill -TERM "${pids[@]}" 2>/dev/null || true
    for _ in {1..30}; do
        local still_running=()
        for pid in "${pids[@]}"; do
            kill -0 "$pid" 2>/dev/null && still_running+=("$pid")
        done
        ((${#still_running[@]} == 0)) && return
        sleep 1
    done

    printf 'Force-stopping remaining Pumpkin processes: %s\n' "${still_running[*]}" >&2
    kill -KILL "${still_running[@]}" 2>/dev/null || true
}

resolve_player_uuid() {
    if [[ -n "$PLAYER_UUID" ]]; then
        return
    fi

    local profile
    profile="$(curl --fail --silent --show-error --max-time 10 \
        "https://api.mojang.com/users/profiles/minecraft/$PLAYER_NAME" || true)"
    PLAYER_UUID="$(jq -r '.id // empty' <<<"$profile")"
    if [[ -z "$PLAYER_UUID" ]]; then
        local existing_ops="$RUN_DIR/data/ops.json"
        if [[ -f "$existing_ops" ]]; then
            PLAYER_UUID="$(jq -r --arg name "$PLAYER_NAME" \
                '.[] | select(.name == $name) | .uuid' "$existing_ops" | head -n 1)"
        fi
    fi
    [[ -n "$PLAYER_UUID" ]] || {
        printf 'Could not resolve the UUID for %s. Set PUMPKIN_ENTITY_PACKET_UUID.\n' \
            "$PLAYER_NAME" >&2
        exit 1
    }

    local compact_uuid="${PLAYER_UUID//-/}"
    if [[ "$compact_uuid" =~ ^[0-9a-fA-F]{32}$ ]]; then
        PLAYER_UUID="${compact_uuid:0:8}-${compact_uuid:8:4}-${compact_uuid:12:4}-${compact_uuid:16:4}-${compact_uuid:20:12}"
    else
        printf 'Invalid UUID for %s: %s\n' "$PLAYER_NAME" "$PLAYER_UUID" >&2
        exit 1
    fi
}

configure_operator() {
    local ops_file="$RUN_DIR/data/ops.json"
    local tmp_file
    mkdir -p "$RUN_DIR/data"
    [[ -f "$ops_file" ]] || printf '%s\n' '[]' >"$ops_file"
    tmp_file="$(mktemp "$ops_file.tmp.XXXXXX")"
    jq --arg uuid "$PLAYER_UUID" --arg name "$PLAYER_NAME" \
        'map(select(.uuid != $uuid and .name != $name)) +
         [{uuid: $uuid, name: $name, level: 4, bypasses_player_limit: false}]' \
        "$ops_file" >"$tmp_file"
    mv -- "$tmp_file" "$ops_file"
}

stop_existing_servers
resolve_player_uuid
configure_operator

printf 'Building Pumpkin release binary...\n'
env CARGO_INCREMENTAL=0 cargo build --release -p pumpkin --manifest-path "$REPO_ROOT/Cargo.toml"

mkdir -p "$RUN_DIR"
rm -f -- "$PID_FILE"
printf 'Launching clean Pumpkin instance from %s\n' "$RUN_DIR"
(
    cd -- "$RUN_DIR"
    nohup "$SERVER_BINARY" >"$SERVER_LOG" 2>&1 < /dev/null &
    printf '%s\n' "$!" >"$PID_FILE"
)

printf 'Pumpkin started for %s (OP level 4); log: %s\n' "$PLAYER_NAME" "$SERVER_LOG"
