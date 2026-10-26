#!/usr/bin/env bash
# 本地开发编排：启动/停止 demo_server + 本地 nginx。
#   scripts/dev.sh up      构建并启动（demo_server :9000，nginx :8080）
#   scripts/dev.sh down    停止两者
#   scripts/dev.sh restart 重启（改完 .so 后更快验证）
#   scripts/dev.sh status  查看端口与健康检查
#   scripts/dev.sh logs    跟踪两个日志
set -euo pipefail

# 本地测试永远不走代理（http_proxy 只用于下载依赖/源码）
export NO_PROXY="127.0.0.1,localhost,::1"
export no_proxy="$NO_PROXY"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFIX="$ROOT/nginx"
RUN="$ROOT/.run"
DEMO_PID="$RUN/demo_server.pid"
NGINX_PID="$PREFIX/logs/nginx.pid"

mkdir -p "$RUN" "$PREFIX/logs/tmp" "$PREFIX/modules"

demo_running() { [ -f "$DEMO_PID" ] && kill -0 "$(cat "$DEMO_PID")" 2>/dev/null; }
port_busy() { ss -ltn 2>/dev/null | grep -q "127.0.0.1:$1 "; }

# 本仓库的 nginx master 进程。pid 文件只是缓存：master 退出时会无条件 unlink 该文件，
# 多 master 并存（pid 文件丢失时 down 没停掉旧 master）就会把新 master 的记录删掉。
# 所以这里以 /proc/<pid>/exe 为准，pid 文件丢了也能找回真实进程。
nginx_masters() {
  local pid exe
  for pid in $(pgrep -x nginx 2>/dev/null || true); do
    exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
    exe="${exe% (deleted)}"
    [ "$exe" = "$PREFIX/sbin/nginx" ] || continue
    tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q 'master process' || continue
    echo "$pid"
  done
}

# 活着就以真实 master 为准，并顺手把 pid 文件写回去（-s reload/quit 需要它）。
# 反过来也重要：绝不把 pid 文件里可能已被复用的 pid 当成 nginx。
nginx_running() {
  local pid
  pid="$(nginx_masters | head -n 1)"
  if [ -z "$pid" ]; then
    [ -f "$NGINX_PID" ] && rm -f "$NGINX_PID"
    return 1
  fi
  [ "$(cat "$NGINX_PID" 2>/dev/null || true)" = "$pid" ] || echo "$pid" >"$NGINX_PID"
  return 0
}

# pid 文件可能丢失/过期（例如终端退出后进程仍在）。只认本仓库的 demo_server：
# 用 /proc/<pid>/exe 比对真实可执行文件，不受启动时是相对路径还是绝对路径影响。
demo_orphans() {
  local pid exe
  for pid in $(pgrep -f 'demo_server' 2>/dev/null || true); do
    exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
    exe="${exe% (deleted)}"
    case "$exe" in
      "$ROOT/target/debug/demo_server" | "$ROOT/target/release/demo_server") echo "$pid" ;;
    esac
  done
}

# 不在 pid 文件里的本仓库 demo_server（用于 status 提示）
demo_strays() {
  local pid recorded=""
  [ -f "$DEMO_PID" ] && recorded="$(cat "$DEMO_PID" 2>/dev/null || true)"
  for pid in $(demo_orphans); do
    [ "$pid" = "$recorded" ] && continue
    echo "$pid"
  done
}

# 停掉 pid（先 TERM，超时再 KILL）。返回 0 表示已退出。
kill_wait() {
  local pid="$1" i
  kill "$pid" 2>/dev/null || return 0
  for i in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.1; done
  kill -9 "$pid" 2>/dev/null || true
  for i in $(seq 1 10); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.1; done
  return 1
}

stop_demo() {
  local pid
  if demo_running; then
    pid="$(cat "$DEMO_PID")"
    echo "==> 停止 demo_server (pid $pid)"
    kill_wait "$pid" || echo "警告：demo_server (pid $pid) 未能退出" >&2
  fi
  rm -f "$DEMO_PID"

  # 兜底：pid 文件失效时，按可执行文件路径清理本仓库的残留进程
  for pid in $(demo_orphans); do
    echo "==> 清理残留 demo_server (pid $pid)"
    kill_wait "$pid" || echo "警告：残留 demo_server (pid $pid) 未能退出" >&2
  done
}

up() {
  [ -x "$PREFIX/sbin/nginx" ] || { echo "缺少本地 nginx，先跑 scripts/build-nginx.sh" >&2; exit 1; }
  [ -f "$PREFIX/modules/libmy_gateway.so" ] || { echo "缺少模块，先跑 scripts/build-module.sh" >&2; exit 1; }

  # pid 文件活着但端口没起，说明是过期记录（或进程卡住），清掉重来
  if demo_running && ! port_busy 9000; then
    echo "==> pid 文件与端口不一致，清理后重启"
    stop_demo
  fi

  if ! demo_running; then
    if port_busy 9000; then
      # 端口被占但没有有效 pid 文件：先尝试收掉本仓库的孤儿进程
      if [ -n "$(demo_orphans)" ]; then
        stop_demo
        sleep 0.2
      fi
      if port_busy 9000; then
        echo "端口 9000 已被其他进程占用：" >&2
        ss -ltnp 2>/dev/null | grep '127.0.0.1:9000 ' >&2 || true
        echo "先确认并停止它（例如 kill <pid>），再重试。" >&2
        exit 1
      fi
    fi
    # 二进制不存在，或 demo_server 源码比二进制新（改完代码 restart 时自动重编，避免跑旧程序）
    if [ ! -x "$ROOT/target/debug/demo_server" ] \
      || [ -n "$(find "$ROOT/demo_server/src" -name '*.rs' -newer "$ROOT/target/debug/demo_server" -print -quit)" ]; then
      echo "==> 构建 demo_server（--offline，避免本地启动时访问网络）"
      cargo build --offline -p demo_server --manifest-path "$ROOT/Cargo.toml" >/dev/null
    fi

    echo "==> 启动 demo_server :9000"
    rm -f "$DEMO_PID"
    # setsid --fork：强制脱离当前会话/进程组，终端或 harness 退出不会带走进程，
    # 也不会留下一个 wait 子进程（旧写法 `( cmd & )` 会遗留子 shell，挂住 stdout 管道）。
    # 不能用 $!：--fork 后 $! 是 setsid 的 pid，真实 pid 由 demo_orphans 按 /proc/exe 认领。
    setsid --fork "$ROOT/target/debug/demo_server" >>"$RUN/demo_server.log" 2>&1 </dev/null &

    for _ in $(seq 1 50); do
      curl -sf --max-time 1 --noproxy '*' http://127.0.0.1:9000/api/health >/dev/null 2>&1 && break
      sleep 0.1
    done

    # 不用 $!：setsid 可能 fork，$! 会是中间 shell。直接按 /proc/exe 认领真实进程。
    local found
    found="$(demo_orphans | head -n 1)"
    if [ -n "$found" ]; then
      echo "$found" >"$DEMO_PID"
    else
      echo "demo_server 启动失败，最近日志：" >&2
      tail -n 10 "$RUN/demo_server.log" >&2 || true
      exit 1
    fi
  else
    echo "==> demo_server 已在运行 (pid $(cat "$DEMO_PID"))"
  fi

  echo "==> 检查 nginx 配置"
  "$PREFIX/sbin/nginx" -p "$PREFIX" -c conf/nginx.conf -t

  if nginx_running; then
    echo "==> reload nginx"
    "$PREFIX/sbin/nginx" -p "$PREFIX" -c conf/nginx.conf -s reload
  else
    echo "==> 启动 nginx :8080"
    "$PREFIX/sbin/nginx" -p "$PREFIX" -c conf/nginx.conf
  fi

  # 等契约拉取完成再报就绪：冷启动/刚 reload 时可能短暂 503（fail-closed 的正常表现）
  local code=""
  for _ in $(seq 1 40); do
    code="$(curl -sS -o /dev/null -w '%{http_code}' --max-time 1 --noproxy '*' \
      http://127.0.0.1:8080/api/health 2>/dev/null || true)"
    [ "$code" = "200" ] && break
    sleep 0.1
  done

  if [ "$code" = "200" ]; then
    echo "==> 就绪：http://127.0.0.1:8080  (后端直连 http://127.0.0.1:9000)"
  else
    echo "==> 已启动，但 /api/health 返回 ${code:-000}（契约可能尚未拉到，看 ./scripts/dev.sh logs）" >&2
  fi
}
down() {
  local pid
  if nginx_running; then
    echo "==> 停止 nginx"
    "$PREFIX/sbin/nginx" -p "$PREFIX" -c conf/nginx.conf -s quit || true
  fi

  # 兜底：pid 文件丢失/过期时，按可执行文件路径清理所有本仓库 master（含 pid 文件被旧
  # master 删掉的孤儿）。先 QUIT，必要时再 TERM/KILL。
  for pid in $(nginx_masters); do
    echo "==> 清理残留 nginx master (pid $pid)"
    kill -QUIT "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    kill_wait "$pid" || echo "警告：nginx master (pid $pid) 未能退出" >&2
  done

  # 确认没有 master 了再删 pid 文件，避免删掉仍在运行的 master 的记录
  if [ -z "$(nginx_masters)" ]; then
    rm -f "$NGINX_PID"
  else
    echo "警告：仍有 nginx master 未退出：$(nginx_masters | tr '\n' ' ')" >&2
  fi

  stop_demo

  if port_busy 9000; then
    echo "警告：9000 端口仍被占用（不属于本仓库的 demo_server）" >&2
    ss -ltnp 2>/dev/null | grep '127.0.0.1:9000 ' >&2 || true
  fi
  if port_busy 8080; then
    echo "警告：8080 端口仍被占用" >&2
    ss -ltnp 2>/dev/null | grep '127.0.0.1:8080 ' >&2 || true
  fi
  echo "==> 已停止"
}

status() {
  local had_pid_file=0
  [ -f "$NGINX_PID" ] && had_pid_file=1
  echo -n "nginx:        "
  if nginx_running; then
    echo "running (pid $(cat "$NGINX_PID"))"
    [ "$had_pid_file" = 1 ] || echo "              (pid 文件缺失，已按真实进程回写)"
  else
    echo "stopped"
  fi
  echo -n "demo_server:  "; demo_running && echo "running (pid $(cat "$DEMO_PID"))" || echo "stopped"
  local orphans
  orphans="$(demo_strays)"
  if [ -n "$orphans" ]; then
    echo "孤儿进程:     $(echo "$orphans" | tr '\n' ' ')(不在 pid 文件里，跑 down 可清理)"
  fi
  echo -n "gateway 健康: "; curl -sS --max-time 2 -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/api/health 2>/dev/null || echo 000
  echo -n "后端健康:     "; curl -sS --max-time 2 -o /dev/null -w '%{http_code}\n' http://127.0.0.1:9000/api/health 2>/dev/null || echo 000
}

case "${1:-}" in
  up) up ;;
  down) down ;;
  restart) down; up ;;
  status) status ;;
  logs) tail -n 50 -f "$PREFIX/logs/error.log" "$RUN/demo_server.log" ;;
  *) echo "用法: $0 {up|down|restart|status|logs}" >&2; exit 2 ;;
esac
