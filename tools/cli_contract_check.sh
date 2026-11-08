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

# ---- 平台判定（Windows 上是 Git Bash / MSYS2）----
case "$(uname -s 2>/dev/null || echo unknown)" in
  MINGW* | MSYS* | CYGWIN* | Windows_NT) IS_WINDOWS=1 ;;
  *) IS_WINDOWS=0 ;;
esac

# ---- 二进制（Windows 上是 .exe）----
#
# ⚠️ **要认 CARGO_TARGET_DIR**：Windows 上为了躲 MAX_PATH(260)，
#    CI 把 target 目录挪到了别处（`D:\t`）⇒ 二进制**不在** `target/release/` 下。
#    （这个坑到第 8 轮才暴露：前几轮都在这一步之前就挂了，契约从没真跑过。）
# ⚠️ 也允许用 `RBIN=` 环境变量直接指定（最省事）。
RBIN="${RBIN:-}"
if [ -z "$RBIN" ]; then
  if [ "$IS_WINDOWS" = "1" ]; then
    RBIN="target/release/puttyplus.exe"
  else
    RBIN="target/release/puttyplus"
  fi
  if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    # ⚠️ CARGO_TARGET_DIR 可能是 Windows 形式（`D:\t`）⇒ 归一化成正斜杠
    #    （Git Bash 里 `D:\t/release/x.exe` 这种混合写法不稳）
    CTD="${CARGO_TARGET_DIR//\\//}"
    if [ "$IS_WINDOWS" = "1" ]; then
      RBIN="$CTD/release/puttyplus.exe"
    else
      RBIN="$CTD/release/puttyplus"
    fi
  fi
fi
# 退路：上面那个不在，但默认路径下有 ⇒ 用默认路径的
if [ ! -x "$RBIN" ] && [ -x "target/release/puttyplus.exe" ]; then
  RBIN="target/release/puttyplus.exe"
fi
if [ ! -x "$RBIN" ] && [ -x "target/release/puttyplus" ]; then
  RBIN="target/release/puttyplus"
fi

# ---- 归一化用的「真实身份」（在覆盖 HOME **之前**取）----
#
# ⚠️ **这里取的是真 home，不是 fixture home**：
#    脚本里的 `~` 是在**脚本的 shell** 里展开的（eval 展开）⇒ 展开成的是
#    「跑脚本那个用户」的真实 home，而不是 fixture ⇒ 输出的路径形状
#    随平台而变 ⇒ 必须归一化成同一个形状（`/home/<ME>`）才能与基准对上。
#
#    - Linux：真实 home 是 `/home/<user>`，MSYS 的 `~` 也展开成这个；
#    - Windows（Git Bash）：`~` 展开成 **MSYS 风格** `/c/Users/<user>`
#      （不是 `C:\Users\<user>`！）⇒ 两种形态都要登记。
ME=""
for cand in "$(id -un 2>/dev/null)" "${USERNAME:-}" "${USER:-}"; do
  if [ -n "$cand" ]; then ME="$cand"; break; fi
done
if [ -z "$ME" ] && [ -n "${USERPROFILE:-}" ]; then
  ME="${USERPROFILE##*/}"
fi
[ -n "$ME" ] || ME="unknown"

# 真实 home 的各种形态（归一化时逐个替换成 `/home/<ME>`）。
# ⚠️ 顺序无所谓（长度不同也不会互相误伤，因为都替换成同一个目标）。
HOME_FORMS=()
add_home_form() {  # 一个形态
  local h="$1"
  [ -n "$h" ] || return 0
  h="${h//\\//}"          # 反斜杠先转正斜杠
  HOME_FORMS+=("$h")
}
add_home_form "${HOME:-}"                # Linux: /home/<user>；Git Bash: /c/Users/<user>
add_home_form "${USERPROFILE:-}"         # Windows 原生: C:\Users\<user>
if [ "$IS_WINDOWS" = "1" ] && [ -n "${USERPROFILE:-}" ] && command -v cygpath >/dev/null 2>&1; then
  add_home_form "$(cygpath -u "$USERPROFILE" 2>/dev/null)"   # /c/Users/<user>
fi

# ---- fixture HOME（会话文件 + ssh config）----
#
# ⚠️ **Unix 走 HOME，Windows 走 USERPROFILE**：
#    GLib 的 `g_get_home_dir()` 在 Windows 上读的是 `USERPROFILE`；
#    而且 MSYS 的 `HOME` 是 `/c/Users/…` 这种 **MSYS 风格**路径，
#    原生 Windows 程序拿它是错的（会当成当前盘根目录下的 \c\Users\…）。
#    ⇒ 两个变量都给成 fixture 目录，程序读哪个都对。
if [ "$IS_WINDOWS" = "1" ]; then
  FIX_HOME_NATIVE="${TEMP:-C:/Windows/Temp}\\pp_cli_contract_home"
  if command -v cygpath >/dev/null 2>&1; then
    FIX_HOME_MSYS="$(cygpath -u "$FIX_HOME_NATIVE" 2>/dev/null || printf '%s' "$FIX_HOME_NATIVE")"
  else
    FIX_HOME_MSYS="$FIX_HOME_NATIVE"
  fi
else
  FIX_HOME_NATIVE="/tmp/pp_cli_contract_home"
  FIX_HOME_MSYS="/tmp/pp_cli_contract_home"
fi
FIX_HOME="$FIX_HOME_MSYS"   # 脚本自己读写用

# ---- 找参考实现二进制（只有 --gen 重新生成基准时才用）----
C_BIN=""
for c in ./reference/puttyplus ./reference/puttyplus.exe ./puttyplus; do
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

# ---- 归一化：把「平台差异」抹平成基准里的形状 ----
#
# 做三件事（顺序不能换）：
#   1. 反斜杠 → 正斜杠（Windows 路径）；
#   2. 真实 home 的各种形态 → `/home/<ME>`（Windows 上是 `C:/Users/<user>`
#      或 MSYS 的 `/c/Users/<user>`，Linux 上是 `/home/<user>`）；
#   3. 用户名 → `<ME>`（默认用户那一格）。
#
# ⚠️ 第 2 步必须在第 3 步**之前**：home 路径里含用户名，先替换 home 才
#    不会把 home 拆成 `/home/<ME>` 之外的形状。
#
# ⚠️ 形态列表与用户名**作为参数传进来**（不直接用全局）⇒ `--selftest`
#    能喂假形态验证归一化本身没坏。
normalize_with() {  # 文本 | 形态列表(换行分隔) | 用户名
  local s="$1" forms="$2" me="$3"
  s=${s//\\//}
  local h
  while IFS= read -r h; do
    [ -n "$h" ] || continue
    s=${s//"$h"/\/home\/<ME>}
  done <<<"$forms"
  [ -n "$me" ] && s=${s//"$me"/<ME>}
  printf '%s' "$s"
}

normalize() {
  local forms
  forms=$(printf '%s\n' ${HOME_FORMS[@]+"${HOME_FORMS[@]}"})
  normalize_with "$1" "$forms" "$ME"
}

# ---- 归一化自测（喂假形态，验证逻辑本身没坏）----
#
# ⚠️ **为什么要单独测**：Windows 上跑契约时，73 条全 DIFF 有两种可能
#    ——「程序行为变了」或「归一化坏了」。先跑这个自测，能立刻分清。
selftest() {
  local fails=0
  local linux_forms="/home/alice"
  local win_forms="C:/Users/alice
/c/Users/alice"

  local out
  out=$(normalize_with "keyfile=/home/alice/.ssh/id_rsa user=alice" "$linux_forms" "alice")
  if [ "$out" != "keyfile=/home/<ME>/.ssh/id_rsa user=<ME>" ]; then
    echo "  [FAIL] Linux 形态归一化不对：$out"; fails=$((fails+1))
  fi

  out=$(normalize_with "keyfile=/c/Users/alice/.ssh/id_rsa user=alice" "$win_forms" "alice")
  if [ "$out" != "keyfile=/home/<ME>/.ssh/id_rsa user=<ME>" ]; then
    echo "  [FAIL] Windows(MSYS) 形态归一化不对：$out"; fails=$((fails+1))
  fi

  out=$(normalize_with "keyfile=C:\\Users\\alice\\.ssh\\id_rsa user=alice" "$win_forms" "alice")
  if [ "$out" != "keyfile=/home/<ME>/.ssh/id_rsa user=<ME>" ]; then
    echo "  [FAIL] Windows(原生) 形态归一化不对：$out"; fails=$((fails+1))
  fi

  # 与 home 无关的路径不许被动（`/keys/id_rsa`、`/dev/ttyUSB0`…）
  out=$(normalize_with "keyfile=/keys/id_rsa remote_cmd=/dev/ttyUSB0" "$linux_forms" "alice")
  if [ "$out" != "keyfile=/keys/id_rsa remote_cmd=/dev/ttyUSB0" ]; then
    echo "  [FAIL] 无关路径被误改：$out"; fails=$((fails+1))
  fi

  echo "== 归一化自测: FAIL=$fails =="
  [ "$fails" -eq 0 ]
}

# ---- 跑一个用例，输出规范化后的结果块 ----
run_case() {  # argv
  local argv="$1"
  local out err ec
  out=$(eval "HOME='$FIX_HOME_NATIVE' USERPROFILE='$FIX_HOME_NATIVE' \"\$BIN\" --print-config $argv" 2>"$FIX_HOME/../pp_cli_err.txt")
  ec=$?
  err=$(cat "$FIX_HOME/../pp_cli_err.txt" 2>/dev/null)
  out=$(normalize "$out")
  err=$(normalize "$err")
  printf -- '--- argv ---\n%s\n--- stdout ---\n%s\n--- stderr ---\n%s\n--- exit ---\n%s\n' \
    "$argv" "$out" "$err" "$ec"
}

# ⚠️ 用 printf 而不是 echo：echo 会多一个换行，tr -c 会把换行也换成 '_'
#    ⇒ 文件名后面多一个下划号（难认，还容易对不上）
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
  --selftest) selftest ;;
  *) check ;;
esac
