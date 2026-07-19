#!/usr/bin/env bash

# 在 zq-platform Linux 宿主机上安装并配置离线 chrony/NTP 服务。
# 本脚本不会操作 Docker 容器，只修改宿主机 chrony 和已启用的防火墙。

set -Eeuo pipefail

readonly MANAGED_BEGIN="# BEGIN audio-latency managed block"
readonly MANAGED_END="# END audio-latency managed block"

ALLOW_CIDRS=()
ALLOW_CIDRS_EXPLICIT=0
BIND_ADDRESS=""
LOCAL_STRATUM=8
DRY_RUN=0
CONFIG_PATH=""
SERVICE_NAME=""
BACKUP_PATH=""
FIREWALL_KIND=""
FIREWALL_RULES_ADDED=()

log() {
    printf '[INFO] %s\n' "$*"
}

warn() {
    printf '[WARN] %s\n' "$*" >&2
}

die() {
    printf '[ERROR] %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
用法：
  sudo bash install-chrony-server.sh \
    [--bind-address 192.168.10.20] \
    [--local-stratum 8] \
    [--allow-cidr 192.168.10.0/24] \
    [--dry-run]

参数：
  --allow-cidr CIDR       允许访问 NTP 的网段，可重复指定；不传时开放所有 IPv4/IPv6 网段
  --bind-address IP       chrony 监听的宿主机 IP，可选
  --local-stratum N       离线本地时钟层级，1 到 15，默认 8
  --dry-run               只检查参数并打印计划，不安装或修改系统
  -h, --help              显示帮助
EOF
}

validate_token() {
    local name="$1"
    local value="$2"

    [[ -n "$value" ]] || die "$name 不能为空"
    [[ "$value" != *$'\n'* && "$value" != *$'\r'* ]] || die "$name 包含非法换行"
    [[ "$value" =~ ^[0-9A-Fa-f:.]+(/[0-9]{1,3})?$ ]] || die "$name 格式无效: $value"
}

parse_args() {
    while (($# > 0)); do
        case "$1" in
            --allow-cidr)
                (($# >= 2)) || die "--allow-cidr 缺少参数"
                validate_token "CIDR" "$2"
                if ((ALLOW_CIDRS_EXPLICIT == 0)); then
                    ALLOW_CIDRS=()
                    ALLOW_CIDRS_EXPLICIT=1
                fi
                ALLOW_CIDRS+=("$2")
                shift 2
                ;;
            --bind-address)
                (($# >= 2)) || die "--bind-address 缺少参数"
                validate_token "监听地址" "$2"
                [[ "$2" != */* ]] || die "--bind-address 必须是 IP，不能包含 CIDR 掩码"
                BIND_ADDRESS="$2"
                shift 2
                ;;
            --local-stratum)
                (($# >= 2)) || die "--local-stratum 缺少参数"
                [[ "$2" =~ ^[0-9]+$ ]] || die "--local-stratum 必须是整数"
                LOCAL_STRATUM="$2"
                shift 2
                ;;
            --dry-run)
                DRY_RUN=1
                shift
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

    if ((${#ALLOW_CIDRS[@]} == 0)); then
        ALLOW_CIDRS=("0.0.0.0/0" "::/0")
    fi
    ((LOCAL_STRATUM >= 1 && LOCAL_STRATUM <= 15)) || die "--local-stratum 必须在 1 到 15 之间"
}

print_plan() {
    log "部署计划："
    if ((ALLOW_CIDRS_EXPLICIT)); then
        log "  允许网段: ${ALLOW_CIDRS[*]}（按参数限制）"
    else
        log "  允许网段: ${ALLOW_CIDRS[*]}（默认开放所有 IPv4/IPv6 来源）"
    fi
    log "  监听地址: ${BIND_ADDRESS:-由 chrony 默认监听}"
    log "  本地层级: $LOCAL_STRATUM"
    log "  操作范围: chrony 配置、chrony 服务、已启用的 firewalld/ufw"
}

install_package() {
    if command -v chronyd >/dev/null 2>&1 && command -v chronyc >/dev/null 2>&1; then
        log "chrony 已安装，跳过软件包安装和仓库访问"
        return
    fi

    if command -v apt-get >/dev/null 2>&1; then
        log "检测到 apt，安装 chrony"
        DEBIAN_FRONTEND=noninteractive apt-get update
        DEBIAN_FRONTEND=noninteractive apt-get install -y chrony
    elif command -v dnf >/dev/null 2>&1; then
        log "检测到 dnf，安装 chrony"
        dnf install -y chrony
    elif command -v yum >/dev/null 2>&1; then
        log "检测到 yum，安装 chrony"
        yum install -y chrony
    else
        die "不支持的包管理器；请先手工安装 chrony"
    fi
}

detect_config_and_service() {
    if [[ -f /etc/chrony/chrony.conf ]]; then
        CONFIG_PATH=/etc/chrony/chrony.conf
    elif [[ -f /etc/chrony.conf ]]; then
        CONFIG_PATH=/etc/chrony.conf
    else
        die "chrony 已安装，但未找到 /etc/chrony/chrony.conf 或 /etc/chrony.conf"
    fi

    if systemctl list-unit-files chronyd.service --no-legend 2>/dev/null | grep -q '^chronyd.service'; then
        SERVICE_NAME=chronyd
    elif systemctl list-unit-files chrony.service --no-legend 2>/dev/null | grep -q '^chrony.service'; then
        SERVICE_NAME=chrony
    else
        die "未找到 chronyd.service 或 chrony.service"
    fi
}

check_bind_address() {
    [[ -z "$BIND_ADDRESS" ]] && return
    command -v ip >/dev/null 2>&1 || die "指定 --bind-address 时需要 ip 命令"

    if ! ip -o addr show | awk '{print $4}' | cut -d/ -f1 | grep -Fxq "$BIND_ADDRESS"; then
        die "监听地址不属于当前宿主机: $BIND_ADDRESS"
    fi
}

restore_backup() {
    if [[ -n "$BACKUP_PATH" && -f "$BACKUP_PATH" && -n "$CONFIG_PATH" ]]; then
        warn "恢复 chrony 配置备份: $BACKUP_PATH"
        cp -a "$BACKUP_PATH" "$CONFIG_PATH"
        if [[ -n "$SERVICE_NAME" ]]; then
            warn "尝试使用原配置重新启动 $SERVICE_NAME"
            systemctl restart "$SERVICE_NAME" || warn "原服务未能自动恢复，请人工检查"
        fi
    fi
}

rollback_firewall() {
    ((${#FIREWALL_RULES_ADDED[@]} > 0)) || return

    warn "清理本次新增的防火墙规则"
    local item
    case "$FIREWALL_KIND" in
        firewalld)
            for item in "${FIREWALL_RULES_ADDED[@]}"; do
                firewall-cmd --permanent --remove-rich-rule="$item" >/dev/null 2>&1 || true
            done
            firewall-cmd --reload >/dev/null 2>&1 || true
            ;;
        ufw)
            for item in "${FIREWALL_RULES_ADDED[@]}"; do
                ufw --force delete allow proto udp from "$item" to any port 123 >/dev/null 2>&1 || true
            done
            ;;
    esac
}

on_error() {
    local exit_code=$?
    trap - ERR
    rollback_firewall
    restore_backup
    die "部署失败，退出码: $exit_code"
}

write_managed_config() {
    local timestamp temp_path
    timestamp="$(date +%Y%m%d-%H%M%S)"
    BACKUP_PATH="${CONFIG_PATH}.audio-latency.${timestamp}.bak"
    temp_path="$(mktemp "${CONFIG_PATH}.audio-latency.XXXXXX")"

    cp -a "$CONFIG_PATH" "$BACKUP_PATH"
    log "已备份原配置: $BACKUP_PATH"

    # 删除上一次受管配置块，保留用户和发行版的其他配置。
    awk -v begin="$MANAGED_BEGIN" -v end="$MANAGED_END" '
        $0 == begin { managed = 1; next }
        $0 == end { managed = 0; next }
        !managed { print }
    ' "$CONFIG_PATH" > "$temp_path"

    {
        printf '\n%s\n' "$MANAGED_BEGIN"
        printf 'local stratum %s\n' "$LOCAL_STRATUM"
        printf 'makestep 1.0 3\n'
        printf 'rtcsync\n'
        if [[ -n "$BIND_ADDRESS" ]]; then
            printf 'bindaddress %s\n' "$BIND_ADDRESS"
        fi
        local cidr
        for cidr in "${ALLOW_CIDRS[@]}"; do
            printf 'allow %s\n' "$cidr"
        done
        printf '%s\n' "$MANAGED_END"
    } >> "$temp_path"

    chmod --reference="$CONFIG_PATH" "$temp_path" 2>/dev/null || chmod 0644 "$temp_path"
    chown --reference="$CONFIG_PATH" "$temp_path" 2>/dev/null || true
    mv "$temp_path" "$CONFIG_PATH"
}

validate_chrony_config() {
    if ! command -v chronyd >/dev/null 2>&1; then
        warn "chronyd 命令不存在"
        return 1
    fi

    # chronyd 4.x 支持用 -p 解析并打印配置，部分旧版本没有该选项。
    if ! chronyd -h 2>&1 | grep -Eq '(^|[[:space:],])-p([[:space:],]|$)'; then
        warn "当前 chronyd 不支持 -p，跳过配置预校验；将通过服务重启结果校验配置"
        return 0
    fi

    log "校验 chrony 配置: $CONFIG_PATH"
    chronyd -p -f "$CONFIG_PATH" >/dev/null
}

configure_firewalld() {
    local cidr rule
    FIREWALL_KIND=firewalld
    for cidr in "${ALLOW_CIDRS[@]}"; do
        rule="rule family=\"$( [[ "$cidr" == *:* ]] && printf ipv6 || printf ipv4 )\" source address=\"$cidr\" port port=\"123\" protocol=\"udp\" accept"
        if firewall-cmd --permanent --query-rich-rule="$rule" >/dev/null 2>&1; then
            log "firewalld 规则已存在: $cidr -> UDP/123"
        else
            firewall-cmd --permanent --add-rich-rule="$rule"
            FIREWALL_RULES_ADDED+=("$rule")
        fi
    done
    firewall-cmd --reload
}

configure_ufw() {
    local cidr
    FIREWALL_KIND=ufw
    for cidr in "${ALLOW_CIDRS[@]}"; do
        if ufw status | grep -F "123/udp" | grep -Fq "$cidr"; then
            log "ufw 规则已存在: $cidr -> UDP/123"
        else
            ufw allow proto udp from "$cidr" to any port 123 comment 'audio-latency-ntp'
            FIREWALL_RULES_ADDED+=("$cidr")
        fi
    done
}

configure_firewall() {
    if command -v firewall-cmd >/dev/null 2>&1 && systemctl is-active --quiet firewalld; then
        log "配置 firewalld，允许 ${ALLOW_CIDRS[*]} 访问 UDP/123"
        configure_firewalld
    elif command -v ufw >/dev/null 2>&1 && ufw status | grep -q '^Status: active'; then
        log "配置 ufw，允许 ${ALLOW_CIDRS[*]} 访问 UDP/123"
        configure_ufw
    else
        warn "未检测到正在运行的 firewalld 或 ufw；未修改防火墙"
        warn "如果宿主机使用其他防火墙，请手工允许 ${ALLOW_CIDRS[*]} 访问 UDP/123"
    fi
}

start_service() {
    log "启用并重启 $SERVICE_NAME"
    systemctl enable "$SERVICE_NAME"
    systemctl restart "$SERVICE_NAME"
    if ! systemctl is-active --quiet "$SERVICE_NAME"; then
        warn "$SERVICE_NAME 未正常启动"
        return 1
    fi
}

check_udp_listener() {
    if ! command -v ss >/dev/null 2>&1; then
        warn "缺少 ss 命令，无法检查 UDP 监听"
        return 1
    fi
    if ! ss -H -lun | awk '$5 ~ /:123$/ { found = 1 } END { exit !found }'; then
        warn "chrony 已启动，但未检测到 UDP/123 监听"
        return 1
    fi
}

print_result() {
    local server_hint="$BIND_ADDRESS"
    if [[ -z "$server_hint" ]] && command -v hostname >/dev/null 2>&1; then
        server_hint="$(hostname -I 2>/dev/null | awk '{print $1}')"
    fi
    server_hint="${server_hint:-<宿主机局域网IP>}"

    printf '\n'
    log "chrony/NTP 部署完成"
    log "配置文件: $CONFIG_PATH"
    log "配置备份: $BACKUP_PATH"
    log "服务名称: $SERVICE_NAME"
    log "NTP 地址: $server_hint"
    printf '\nWindows 管理员 PowerShell 下一步命令：\n'
    printf '  w32tm /stripchart /computer:%s /samples:10 /dataonly\n' "$server_hint"
    printf '\n建议继续执行：\n'
    printf '  sudo bash verify-chrony-server.sh --expected-address %s\n' "$server_hint"
}

main() {
    parse_args "$@"
    print_plan

    if ((DRY_RUN)); then
        log "dry-run 完成，未修改系统"
        exit 0
    fi

    ((EUID == 0)) || die "请使用 sudo 或 root 执行本脚本"
    command -v systemctl >/dev/null 2>&1 || die "当前系统不使用 systemd，脚本无法自动部署"

    trap on_error ERR
    install_package
    detect_config_and_service
    check_bind_address
    write_managed_config
    validate_chrony_config
    start_service
    check_udp_listener
    configure_firewall
    trap - ERR

    print_result
}

main "$@"
