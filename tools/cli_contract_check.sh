#!/bin/bash
# 命令行参数契约检查。
#
# 契约来源：PuTTY 的命令行行为（逐条写在 docs/命令行契约.md 里）。
# 黄金基准：tools/cli_contract/golden/*.txt —— 用**参考实现生成一次**，
#           之后这个检查只依赖基准数据，不需要任何外部实现。
#
# 用法：
#   bash tools/cli_contract_check.sh          # 对照黄金基准检查本程序
#   bash tools/cli_contract_check.sh --gen    # 用参考实现重新生成基准
#
# ⚠️ 改 src/args.rs 之前先跑一遍这个检查；改完之后如果基准对不上，
#    要么是**破坏了契约**（必须改回来），要么是**故意扩了对外行为**
#    （那要先想清楚：调用方会看到什么）。
set -u
cd "$(dirname "$0")/.." || exit 2

GOLDEN_DIR="tools/cli_contract/golden"
FIX_HOME="/tmp/pp_cli_contract_home"   # 固定 fixture HOME ⇒ 基准可复现
RBIN="target/release/puttyplus"

# ---- 找参考实现二进制（只有 --gen 重新生成基准时才用）----
C_BIN=""
for c in ./reference/puttyplus ./puttyplus; do
  [ -x "$c" ] || continue
  if cmp -s "$c" "$RBIN" 2>/dev/null; then continue; fi   # 别拿本程序的产物当参考实现
  C_BIN="$c"; break
done

# ---- 用例表（名字 | argv，argv 用 shell 引号写法）----
# 覆盖：协议开关 / 无值开关 / 接受但忽略 / 带值参数 / 位置参数 / -- /
#       未知参数 / 缺值 / atoi 语义 / PuTTY 会话 / ~/.ssh/config 别名
CASES=(
"proto_ssh|-ssh h"
"proto_telnet|-telnet h"
"proto_rlogin|-rlogin h"
"proto_raw|-raw h"
"proto_serial|-serial /dev/ttyUSB0"
"proto_serial_with_port|-serial -P 115200 /dev/ttyUSB0"
"proto_serial_user|-serial ops@/dev/ttyUSB0"
"sw_N|-N h"
"sw_t|-t h"
"sw_T|-T h"
"sw_4|-4 h"
"sw_6|-6 h"
"sw_C|-C h"
"sw_v|-v h"
"sw_batch|-batch h"
"ignored_A|-A h"
"ignored_a|-a h"
"ignored_1|-1 h"
"ignored_2|-2 h"
"ignored_noantispoof|-no-antispoof h"
"ignored_share|-share h"
"ignored_noshare|-noshare h"
"ignored_beep|-beep h"
"ignored_logappend|-logappend h"
"ignored_noagent|-noagent h"
"ignored_agent|-agent h"
"val_P|-P 2222 h"
"val_P_22|-P 22 h"
"val_P_0|-P 0 h"
"val_P_atoi_overflow|-P 99999999999 h"
"val_P_atoi_nan|-P abc h"
"val_P_atoi_neg|-P -1 h"
"val_l|-l ops h"
"val_l_empty|-l '' h"
"val_pw|-pw secret h"
"val_pw_empty|-pw '' h"
"val_i|-i /keys/id_rsa h"
"val_i_tilde|-i ~/.ssh/id_rsa h"
"val_L|-L 9000:127.0.0.1:9000 h"
"val_R|-R 2222:host:22 h"
"val_D|-D 1080 h"
"val_proxycmd|-proxycmd 'nc -X connect -x p:8080 %h %p' h"
"val_hostkey|-hostkey aa:bb h"
"val_sercfg|-sercfg 115200,8n1 h"
"val_sessionlog|-sessionlog /tmp/x.log h"
"val_loghost|-loghost real.name h"
"val_title|-title 窗口名 h"
"val_title_empty|-title '' h"
"val_title_eq|-title=x h"
"val_title_eq_empty|-title="
"pos_host|10.0.0.9"
"pos_user_at_host|ops@10.0.0.9"
"pos_remote_cmd|ops@10.0.0.9 remote cmd here"
"pos_remote_cmd_split|ops@10.0.0.9 echo hi"
"pos_double_at|a@b@c"
"dashdash|-- -weird-host"
"dashdash_extra|-P 2222 -- -weird-host extra"
"unknown|-nosuch h"
"unknown_dashdash|--weird h"
"unknown_then_flag|-nosuch -P 22 h"
"unknown_two|-nosuch -nosuch2 h"
"missing_P|-P"
"missing_l|-l"
"missing_pw|-pw"
"missing_i|-i"
"missing_load|-load"
"session_positional|Demo"
"session_positional_override|Demo 10.9.9.9"
"session_load|-load Demo"
"session_load_override|-load Demo 10.9.9.9"
"sshconfig_alias|demo"
"sshconfig_alias_port|-P 2200 demo"
"mix_all|-ssh -l ops -P 2222 -pw pw -i ~/.ssh/id_rsa -C -v -4 -N -L 9000:127.0.0.1:9000 -R 2222:h:22 -D 1080 -title T -proxycmd 'ssh -W %h:%p gw' -- host.example.com remote cmd"
)

# ---- fixture HOME（会话文件 + ssh config）----
# ⚠️ 只覆盖写这两个文件，不删目录（幂等，且不碰用户自己的 HOME）
setup_fixture() {
  mkdir -p "$FIX_HOME/.putty/sessions" "$FIX_HOME/.ssh"
  cat > "$FIX_HOME/.putty/sessions/Demo" <<'EOF'
HostName=192.168.100.100
PortNumber=2222
UserName=ops
Protocol=ssh
PortForwardings=L9000=127.0.0.1:9000,D1080=,R2222=h:22
PublicKeyFile=~/.ssh/id_rsa
ProxyType=none
EOF
  cat > "$FIX_HOME/.ssh/config" <<'EOF'
Host demo
  HostName 10.1.1.1
  Port 2200
  User dbops
EOF
}

# ---- 跑一个用例，输出规范化后的结果块 ----
run_case() {  # argv
  local argv="$1"
  local out err ec
  out=$(eval "HOME='$FIX_HOME' \"\$BIN\" --print-config $argv" 2>/tmp/pp_cli_err.txt)
  ec=$?
  err=$(cat /tmp/pp_cli_err.txt)
  # 当前登录用户名会出现在「用户没给时取默认用户」的位置 ⇒ 归一化成 <ME>
  local me; me=$(id -un)
  out=${out//$me/<ME>}
  err=${err//$me/<ME>}
  printf -- '--- argv ---\n%s\n--- stdout ---\n%s\n--- stderr ---\n%s\n--- exit ---\n%s\n' \
    "$argv" "$out" "$err" "$ec"
}

# ⚠️ 用 printf 而不是 echo：echo 会多一个换行，tr -c 会把换行也换成 '_'
#    ⇒ 文件名后面多一个下划线（难认，还容易对不上）
slug() { printf '%s' "$1" | tr -c 'A-Za-z0-9._-' '_'; }

# ---- 生成基准（需要参考实现二进制）----
gen_golden() {
  [ -n "$C_BIN" ] || { echo "找不到参考实现二进制（./reference/puttyplus）⇒ 无法生成基准"; exit 2; }
  mkdir -p "$GOLDEN_DIR"
  setup_fixture
  local n=0
  for c in "${CASES[@]}"; do
    local name="${c%%|*}"; local argv="${c#*|}"
    BIN="$C_BIN" run_case "$argv" > "$GOLDEN_DIR/$(slug "$name").txt"
    n=$((n+1))
  done
  echo "已用 $C_BIN 生成 $n 条基准 → $GOLDEN_DIR"
}

# ---- 检查 Rust 版 ----
check() {
  [ -x "$RBIN" ] || { echo "缺少 Rust 版二进制 $RBIN（先 cargo build --release）"; exit 2; }
  setup_fixture
  local pass=0 fail=0
  for c in "${CASES[@]}"; do
    local name="${c%%|*}"; local argv="${c#*|}"
    local f="$GOLDEN_DIR/$(slug "$name").txt"
    if [ ! -f "$f" ]; then
      echo "  [缺基准] $name"; fail=$((fail+1)); continue
    fi
    local actual; actual=$(BIN="$RBIN" run_case "$argv")
    if [ "$actual" = "$(cat "$f")" ]; then
      pass=$((pass+1))
    else
      fail=$((fail+1))
      echo "  [DIFF] $name  ($argv)"
      diff <(echo "$actual") "$f" | head -12
    fi
  done
  echo "== 命令行契约: PASS=$pass FAIL=$fail =="
  [ "$fail" -eq 0 ]
}

case "${1:-}" in
  --gen) gen_golden ;;
  *) check ;;
esac
