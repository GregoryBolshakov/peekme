#!/usr/bin/env python3
"""Run a command in a PTY, drive it with a small script, save every output byte.

    python3 spikes/capture.py out.bin COLS ROWS SCRIPT -- cmd args...

SCRIPT is a '|'-separated list of steps:
    w:<regex>[@secs]   wait until the de-escaped output tail matches (default 60 s)
    s:<secs>           sleep
    k:<python-bytes>   send bytes, e.g. k:b'hello\\r'
    x:<python>         run a statement with `pid` (the command) and `os`, `signal` in scope
Also writes out.bin.log with timestamps of each step and of each read.
"""
import os, pty, re, select, signal, struct, sys, termios, fcntl, time, json

ANSI = re.compile(rb'\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-_]')

def main():
    out, cols, rows, script = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
    cmd = sys.argv[sys.argv.index('--') + 1:]
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('CLAUDECODE', 'CLAUDE_CODE_', 'CLAUDE_PID', 'CLAUDE_EFFORT'))}
    env['TERM'] = env.get('TERM', 'xterm-256color')
    for kv in os.environ.get('CAP_ENV', '').split(','):
        if '=' in kv:
            k, v = kv.split('=', 1); env[k] = v
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
        os.execvpe(cmd[0], cmd, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', rows, cols, 0, 0))
    data = bytearray(); log = []; t0 = time.time()

    def pump(timeout):
        r, _, _ = select.select([fd], [], [], timeout)
        if r:
            try:
                b = os.read(fd, 65536)
            except OSError:
                return False
            if not b:
                return False
            data.extend(b); log.append((round(time.time() - t0, 3), 'read', len(b)))
            # answer cursor position / device attribute queries like a terminal would
            if b'\x1b[6n' in b: os.write(fd, f'\x1b[{rows};1R'.encode())
            if b'\x1b[c' in b or b'\x1b[0c' in b: os.write(fd, b'\x1b[?62;22c')
        return True

    alive = True
    for step in [s for s in script.split('|') if s]:
        kind, arg = step.split(':', 1)
        log.append((round(time.time() - t0, 3), 'step', step, len(data)))
        if kind == 'w':
            pat, _, secs = arg.partition('@'); dl = time.time() + float(secs or 60)
            while alive and time.time() < dl:
                if re.search(pat.encode(), ANSI.sub(b'', bytes(data[-20000:]))):
                    break
                alive = pump(0.1)
            else:
                log.append((round(time.time() - t0, 3), 'timeout', pat))
        elif kind == 's':
            dl = time.time() + float(arg)
            while alive and time.time() < dl:
                alive = pump(max(0, dl - time.time()))
        elif kind == 'k':
            os.write(fd, eval(arg))
        elif kind == 'x':
            exec(arg, {'os': os, 'signal': signal, 'pid': pid, 'log': log})
    dl = time.time() + 1
    while alive and time.time() < dl:
        alive = pump(0.1)
    open(out, 'wb').write(data)
    open(out + '.log', 'w').write('\n'.join(json.dumps(x) for x in log))
    try:
        os.kill(pid, 9)
    except ProcessLookupError:
        pass
    print(f'{len(data)} bytes -> {out}')

main()
