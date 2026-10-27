#!/usr/bin/env python3
"""Exercise production OwnedPty resize and PtyWinsize against Linux PTYs.

Only unrelated object fields/GTK wiring are omitted. The libc adapter forwards
real openpty/ioctl calls, with deterministic per-thread errno injection.
"""
from pathlib import Path
import os
import re
import subprocess
import tempfile
import termios
import textwrap

ROOT = Path(os.environ.get("FORGE_RESIZE_SOURCE", Path(__file__).resolve().parents[1]))


def item(source, signature):
    start = source.index(signature.strip())
    line = source.rfind("\n", 0, start) + 1
    indent = source[line:start]
    opening = source.index("{", start)
    end = re.search(r"^" + re.escape(indent) + r"}[ \t]*$", source[opening:], re.M)
    assert end, signature
    return textwrap.dedent(source[line:opening + end.end()])


pty = (ROOT / 'src/pty.rs').read_text()
view = (ROOT / 'src/block_view/mod.rs').read_text()
methods = [item(pty, '    pub fn resize(&self,')]
for signature in ['    fn resize_with(', '    pub fn report_resize_error(']:
    if signature in pty:
        methods.append(item(pty, signature))
source = (Path(__file__).with_name('pty_resize_harness.rs')).read_text()
source = source.replace('/* METHODS */', '\n'.join(methods))
source = source.replace('/* WINSIZE */', '\n'.join([
    item(view, 'struct PtyWinsize {'), item(view, 'impl PtyWinsize {'),
    item(view, 'fn winsize_to_publish('), item(view, 'fn child_winsize_or_initial(')]))
source = source.replace('/* SET */ 0', str(termios.TIOCSWINSZ)).replace('/* GET */ 0', str(termios.TIOCGWINSZ))
with tempfile.TemporaryDirectory(prefix='forge-pty-resize-') as temp:
    src = Path(temp) / 'test.rs'
    src.write_text(source)
    binary = Path(temp) / 'test'
    flags = ['--cfg', 'f03'] if '    fn resize_with(' in pty else []
    subprocess.run([os.environ.get('RUSTC', 'rustc'), '--edition=2021', '--test', *flags, str(src), '-o', str(binary)], check=True)
    subprocess.run([str(binary), '--test-threads=1', '--nocapture'], check=True)
