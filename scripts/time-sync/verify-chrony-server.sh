#!/usr/bin/env bash

# 验证宿主机 chrony/NTP 服务状态。本脚本只读，不修改系统。

set -Eeuo pipefail

EXPECTED_ADDRESS=""

log() {
    printf '[INFO] %s\n' "$*"
}

die() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
用法：
  bash verify-chrony-server.sh [--expected-address 192.168.10.20]

参数：
  --expected-address IP   预期对外提供 NTP 的宿主机 IP，可选
  -h, --help              显示帮助
EOF
}

parse_args() {
    while (($# > 0)); do
        case "$1" in
            --expected-address)
                (($# >= 2)) || die "--expected-address 缺少参数"
                [[ "$2" =~ ^[0-9A-Fa-f:.]+$ ]] || die "IP 地址格式无效: $2"
                EXPECTED_ADDRESS="$2"
                shift 2
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "未知参数: $1"
                ;;
        esac
    done
}

detect_service() {
    if systemctl is-active --quiet chronyd; then
        printf chronyd
    elif systemctl is-active --quiet chrony; then
        printf chrony
    else
        return 1
    fi
}

check_expected_address() {
    [[ -z "$EXPECTED_ADDRESS" ]] && return
    command -v ip >/dev/null 2>&1 || die "缺少 ip 命令，无法验证宿主机地址"
    ip -o addr show | awk '{print $4}' | cut -d/ -f1 | grep -Fxq "$EXPECTED_ADDRESS" \
        || die "预期地址不属于当前宿主机: $EXPECTED_ADDRESS"
}

check_listener() {
    command -v ss >/dev/null 2>&1 || die "缺少 ss 命令，无法检查 UDP 监听"
    local listeners
    listeners="$(ss -H -lun | awk '$5 ~ /:123$/')"
    [[ -n "$listeners" ]] || die "未检测到 UDP/123 监听"

    log "UDP/123 监听正常："
    printf '%s\n' "$listeners"

    if [[ -n "$EXPECTED_ADDRESS" ]]; then
        if ! printf '%s\n' "$listeners" | grep -Fq "${EXPECTED_ADDRESS}:123" \
            && ! printf '%s\n' "$listeners" | grep -Eq '(^|[[:space:]])(0\.0\.0\.0|\[::\]|\*):123([[:space:]]|$)'; then
            die "UDP/123 未监听预期地址: $EXPECTED_ADDRESS"
        fi
    fi
}

main() {
    parse_args "$@"
    command -v systemctl >/dev/null 2>&1 || die "当前系统不使用 systemd"
    command -v chronyc >/dev/null 2>&1 || die "chronyc 命令不存在，请先安装 chrony"

    local service
    service="$(detect_service)" || die "chronyd/chrony 服务未运行"
    log "服务状态正常: $service"

    check_expected_address
    check_listener

    log "chronyc tracking："
    chronyc tracking
    printf '\n'

    log "chronyc sources -v："
    chronyc sources -v
    printf '\n'

    log "chronyc clients："
    if ((EUID == 0)); then
        chronyc clients
    else
        log "当前不是 root，跳过客户端列表；需要时使用 sudo 执行"
    fi

    printf '\n'
    log "宿主机 chrony/NTP 检查通过"
    if [[ -n "$EXPECTED_ADDRESS" ]]; then
        printf '请在 Windows 管理员 PowerShell 继续验证：\n'
        printf '  w32tm /stripchart /computer:%s /samples:10 /dataonly\n' "$EXPECTED_ADDRESS"
    fi
}

main "$@"
