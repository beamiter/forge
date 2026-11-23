#!/usr/bin/env python3
"""Exercise Forge's application Find on its native shared terminal surfaces."""
import argparse
import json
import os
from pathlib import Path
import tempfile

from native import Atspi, accessible_text, application_nodes, first_named, start_accessibility, wait_for
from native_jsh import application, xdotool


SHELL = r"""#!/usr/bin/python3
import os
from pathlib import Path
import sys
import time
import tty

root = Path(__file__).parent
(root / 'leader').write_text(str(os.getpid()))
tty.setraw(0)

def emit(text):
    os.write(1, text)

def wait_to_enter_alternate_screen():
    deadline = time.monotonic() + 45
    while not (root / 'enter-alternate').exists() and time.monotonic() < deadline:
        time.sleep(0.02)

if root.name == 'unified':
    emit(b'shared-find-probe archived\r\n')
    for row in range(90):
        emit(f'{row}\r\n'.encode())
    emit(b'shared-find-probe live\r\n')
else:
    emit(b'\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;C;cmdline_url=previous\x07')
    emit(b'shared-find-probe finished\r\n')
    emit(b'\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;C;cmdline_url=current\x07')
    emit(b'shared-find-probe live\r\n')
    wait_to_enter_alternate_screen()
    emit(b'\x1b[?1049hfull-screen-find-probe\r\n')
    (root / 'alternate-input').write_bytes(os.read(0, 1))
    emit(b'\x1b[?1049l\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07')
sys.stdin.read(1)
"""


def type_query(query):
    xdotool('type', '--clearmodifiers', '--delay', '2', query)


def run(binary, root, mode):
    root.mkdir(mode=0o700)
    shell = root / 'shell'
    shell.write_text(SHELL)
    shell.chmod(0o700)
    config = root / 'forge.toml'
    config.write_text(f'shell = {json.dumps(str(shell))}\nterminal_mode = "{mode}"\n')
    config.chmod(0o600)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith('JSH_') and key not in ('FORGE_SHELL', 'FORGE_HISTORY_PATH', 'BASH_ENV')}
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR'):
        directory = root / key
        directory.mkdir(mode=0o700)
        env[key] = str(directory)
    env.update(GDK_BACKEND='x11', GSK_RENDERER='cairo', GTK_A11Y='atspi', GTK_IM_MODULE='simple',
               JTERM_SHARED_ACTIVE_TERMINAL='1', JTERM_SHARED_TRANSCRIPT='1')

    with application(binary, config, env, root / 'forge.log', restore=False) as process:
        pid = process.pid

        def screen_text():
            screen = next((node for node in application_nodes(pid)
                           if node.get_description() == 'Shared active terminal'
                           and node.get_state_set().contains(Atspi.StateType.SHOWING)), None)
            return accessible_text(screen)

        def entry(state=Atspi.StateType.FOCUSED):
            return first_named(pid, 'Find in terminal history', state)

        def status(count):
            return entry() and first_named(pid, count)

        wait_for('the live transcript', lambda: 'shared-find-probe live' in screen_text())
        leader = int((root / 'leader').read_text())
        tty_fd = Path(f'/proc/{leader}/fd/0')
        terminal = tty_fd.readlink()
        assert str(terminal).startswith('/dev/pts/')
        if mode == 'unified':
            assert 'shared-find-probe archived' not in screen_text()
            button = wait_for('Unified Find toolbar button', lambda: first_named(pid, 'Find in this terminal'))
            assert Atspi.Action.do_action(button.get_action_iface(), 0)
        else:
            xdotool('key', '--clearmodifiers', 'ctrl+shift+f')
        wait_for('application Find with search focus', entry)
        type_query('shared-find-probe')
        wait_for('literal text in the focused Find entry',
                 lambda: (focused := entry()) and accessible_text(focused) == 'shared-find-probe')
        try:
            wait_for('first of two matches', lambda: status('1 of 2'))
        except RuntimeError:
            labels = [node.get_name() for node in application_nodes(pid)
                      if node.get_state_set().contains(Atspi.StateType.SHOWING)
                      and node.get_role() == Atspi.Role.STATUS_BAR]
            print(f'Visible Find status in {mode}: {labels!r}', flush=True)
            raise
        xdotool('key', '--clearmodifiers', 'Return')
        wait_for('second match on the mounted terminal', lambda: status('2 of 2') and (
            ('shared-find-probe archived' if mode == 'unified' else 'shared-find-probe live') in screen_text()
        ))
        xdotool('key', '--clearmodifiers', 'shift+Return')
        wait_for('previous match', lambda: status('1 of 2') and (
            mode != 'unified' or 'shared-find-probe live' in screen_text()
        ))
        if mode != 'unified':
            (root / 'enter-alternate').touch()
            wait_for('the alternate screen', lambda: 'full-screen-find-probe' in screen_text())
            xdotool('key', '--clearmodifiers', 'Return')
            wait_for('the old query is outside the alternate screen', lambda: status('No matches'))
            xdotool('key', '--clearmodifiers', 'ctrl+a')
            type_query('full-screen-find-probe')
            wait_for('alternate screen match', lambda: status('1 of 1'))
        xdotool('key', '--clearmodifiers', 'Escape')
        wait_for('Find bar closes', lambda: not entry(Atspi.StateType.SHOWING))
        if mode == 'unified':
            xdotool('key', '--clearmodifiers', 'ctrl+shift+f')
            wait_for('application Find from the workspace shortcut', entry)
        else:
            xdotool('key', '--clearmodifiers', 'x')
            input_file = root / 'alternate-input'
            wait_for('the same PTY receives input after Find closes',
                     lambda: input_file.exists() and input_file.read_bytes() == b'x')
        assert tty_fd.readlink() == terminal
    print(f'Forge native shared Find passed in {mode} mode.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--mode', choices=('unified', 'block', 'chat'), action='append')
    args = parser.parse_args()
    start_accessibility()
    with tempfile.TemporaryDirectory(prefix='forge-shared-find-') as directory:
        for mode in args.mode or ('unified', 'block', 'chat'):
            run(args.binary.resolve(strict=True), Path(directory) / mode, mode)
